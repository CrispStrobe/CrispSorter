# crispcat-format

Standalone Rust codec and metadata model for Cathy/Catfish CAF catalogues.
MIT licensed, retaining the original CrispSorter notices. No Tauri, Python,
scanner, content hashing, SQLite, LanceDB, network or native runtime dependency.
The only runtime dependency is serde.

Read CAF versions 1–8; write v6/v8. Supported round trips retain empty folders,
directory timestamps, volume metadata and exact modern zero-byte file sizes.
Unknown legacy size provenance is exposed separately. Existing cooperative
cancellation APIs remain available, and cancelled file publication retains the
previous output.

The library materializes a catalogue in memory. It is an interchange codec,
not the working store for a multi-million-entry catalogue: CrispCloud keeps
that in SQLite. CAF does not encode scan completeness/errors. The existing
Unicode extension uses heuristic UTF-8 detection; classic Cathy/Python exports
must constrain names to Latin-1. No new format or encoding behavior is added by
this extraction.

Until a registry release is deliberately published, consume this crate from
the public Git repository, pinning a reviewed full commit SHA:

```toml
[dependencies]
crispcat-format = { git = "https://github.com/CrispStrobe/CrispSorter", rev = "<reviewed-full-commit>" }
```

```rust,no_run
use crispcat_format::caf;
let index = caf::read_file(std::path::Path::new("archive.caf"))?;
println!("{} entries", index.all_files.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Build and test this manifest independently; application workspace dependencies
are intentionally excluded. CrispSorter's `crispcat` re-exports the same `caf`
and `index` modules to preserve existing callers. CrispCloud's reviewed vendor
copy remains pinned to its independently verified commit until deliberately
updated. This change does not publish to crates.io or relicense existing code.
