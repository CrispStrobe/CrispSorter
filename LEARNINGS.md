# CrispSorter — Learnings & Key Insights

Critical things we've learned that are easy to forget when returning to this codebase.

---

## Build & CI

### `crispembed` optional path dep still needs to resolve
`src-tauri/Cargo.toml` has `crispembed` as an optional dep at path `../../CrispEmbed/crispembed`.
Cargo resolves ALL path deps (even optional ones) during `cargo metadata`, so if the sibling repo
doesn't have the Rust crate, the build fails even when the `crispembed` feature is not enabled.

**Local dev fix:** A minimal stub crate lives at `/Users/<user>/code/CrispEmbed/crispembed/`.
**CI fix:** The release workflow checks out `CrispStrobe/CrispEmbed` and rewrites the Cargo.toml path.

### `src-tauri/target` is a symlink to `<external-volume>`
The Cargo build dir is symlinked to an external volume. If that volume is not mounted, `cargo` fails
with "Not a directory". Fix: `rm src-tauri/target && mkdir src-tauri/target`.

### macOS 13 (Intel) GitHub runner is chronically slow to provision
macOS 13 runners are often queued for 1+ hours on GitHub-hosted Actions. The release workflow now
has a separate `publish` job with `if: always()` that publishes the draft as soon as all *other*
matrix jobs finish — macOS 13 can catch up later or be skipped without blocking the release.

### AppImage requires `APPIMAGE_EXTRACT_AND_RUN=1` on GitHub runners
Linux AppImage tools (linuxdeploy etc.) are themselves AppImages. GitHub runners have no FUSE
mount capability, so they must extract-and-run. Set the env var in the workflow.

---

## Frontend (Svelte 5 + Tauri)

### Svelte 5 rune mode: `$state` inside class fields
`BatchManager` uses `$state<BatchItem[]>([])` as class fields.  This works, but mutations must
happen through direct assignment (`this.items = [...]`) or array mutations (`this.items.push(...)`)
— reactive updates propagate correctly in both cases in Svelte 5.

### `@tauri-apps/plugin-http` fetch supports `AbortSignal`
The Tauri HTTP plugin's `fetch(input, init)` extends `RequestInit`, so `signal: AbortController.signal`
works as expected.  This is NOT documented prominently — it just falls through to the underlying
`reqwest` cancel token.

### `invoke()` calls cannot be cancelled mid-execution
Tauri's `invoke()` is a one-shot IPC call.  Once the Rust handler starts, there is no way to cancel
it from JS.  Use `Promise.race([invokePromise, timeoutPromise, abortPromise])` to make the **JS side**
responsive while the Rust handler runs to completion in the background.

### Stop button only works between async boundaries
`BatchManager.stopRequested` is checked at the top of each loop iteration and after each `await`.
If a single `await` takes a long time (LLM query, Rust extraction), the stop button has no effect
until that `await` resolves.  The fix is to pass `AbortSignal` into the awaited call so it can
terminate early.

### Svelte store subscriptions in rune-mode components
In a `$state`-based (rune-mode) component, subscribe to a Svelte writable store using `$effect`:
```svelte
$effect(() => {
    const unsub = myStore.subscribe(v => { localState = v; });
    return unsub; // cleanup
});
```
Or use `import { get } from 'svelte/store'` for one-shot reads.

---

## Processing Pipeline

### Session resume leaves items in mid-flight statuses
When the app closes mid-batch and resumes, items can be stuck in `extracting` or `analyzing`.
These statuses mean "actively running" to the UI, which is wrong after a restart.
**Pattern:** Always sanitize loaded session items: reset `extracting`/`analyzing` to a safe
"interrupted" status in `resumeLastSession()`.

### Extract-then-analyze is the right two-phase approach
Running extraction and LLM analysis interleaved (per item) means a rate-limited LLM also blocks
all extraction. Better: extract ALL items in pass 1, then analyze ALL in pass 2. Both passes
respect `stopRequested` and can be run independently.

### Per-page watchdog is better than flat timeout for extraction
A 5-minute flat timeout is too generous for small files and too tight for large scanned PDFs.
The right signal is "no page progress in N seconds" — wire the `onProgress` callback to a
watchdog timer that resets on every page event and fires after 30 s of silence.

---

## Rate Limits

### Remote provider 429s can cascade
Rate limits on Groq, OpenRouter, etc. cause the retry loop to eat into `MAX_RL_RETRIES` (currently 6)
before giving up.  During a large batch, this can mean many minutes of dead time.
**Better:** Detect 429 early and switch to a fallback provider (round-robin) rather than retrying
the same one.  Reset the provider index at the start of each `processAll`.

---

## Release

### Version must be bumped in three places
- `package.json` → `"version"`
- `src-tauri/Cargo.toml` → `version = "..."`
- `src-tauri/tauri.conf.json` → `"version"`

All three must match or the Tauri build will fail / produce inconsistent binaries.

### `releaseDraft: true` + separate publish job is the right pattern
`tauri-apps/tauri-action` uploads artifacts to the draft release as each platform job completes.
A separate `publish` job with `needs: [release]` and `if: always()` converts the draft to live
once all matrix jobs have settled — regardless of individual failures.
