# Isolated catalog validation

This small workspace compiles the **actual production** `crates/crispcat`
sources and `crates/crispcat-cli` binary/tests by relative path. It avoids
resolving unrelated desktop, LanceDB, and sibling-repository dependencies.
It does not contain a copy or replacement of the catalog implementation.

From this directory:

```sh
cargo test --locked
```

Only the dependency-light catalog feature set is exercised here. Tauri and
the optional `lance` module require their normal workspace build environment.
