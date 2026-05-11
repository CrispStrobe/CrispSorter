# P13 — Bilder vertical (Photos / images)

> Companion plan for CrispSorter's image-vertical feature.  Lives outside
> [PLAN.md](../PLAN.md) because it spans two repos (CrispSorter +
> sibling [CrispLens](https://github.com/CrispStrobe/CrispLens)).
>
> **Status (2026-05-11):** Tier 1 (A1–A4) + Tier 2 foundation (B1)
> shipped.  Remaining: B2–B5.  See the
> [slice breakdown table](#slice-breakdown-with-hours) for per-slice
> status and the [Spec vs reality appendix](#spec-vs-reality-appendix-2026-05-11)
> for the wire-shape findings that came out of the B1 live cross-check
> against the real CrispLens server.

---

## Goal in one sentence

Ship a **Bilder** tab in Übersicht that's useful on a fresh install with
no extra setup (Tier 1), and that automatically gains semantic image
search + face recognition when a CrispLens server is configured and
reachable (Tier 2) — degrading silently back to Tier 1 when CrispLens
goes offline.

## Non-goals

- Reimplementing CrispLens's editing pipeline (crop / convert / canvas /
  BFL/FLUX image generation).  Those stay in CrispLens's own UI; the
  CrispSorter integration is **read-only enrichment**, with deep-link
  buttons that open CrispLens for write actions.
- Reimplementing CrispLens's face-recognition models in Rust.  That's a
  multi-month effort discussed and rejected in favour of HTTP integration
  (option A in the scoping conversation; see `HISTORY.md`).
- Video files.  Out of scope for v1; can be added later as another
  ext bucket alongside images.
- Mobile.  Tier 1 inherits whatever Tauri mobile state CrispSorter has.

---

## Architecture in one diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                       CrispSorter                                │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │              Übersicht → Bilder tab                       │   │
│  │                                                            │   │
│  │  Tier 1 (always on, no external deps):                    │   │
│  │   - grid of image-row thumbnails from LanceDB index       │   │
│  │   - search by filename / OCR text / EXIF                  │   │
│  │   - folder tree + date histogram (existing infrastructure)│   │
│  │   - SHA-256 dup view (existing P15)                       │   │
│  │   - pHash near-dup view (NEW, small image-hasher crate)   │   │
│  │                                                            │   │
│  │  Tier 2 enhancements (only when CrispLens reachable):     │   │
│  │   - semantic search bar  ───►  POST /api/search           │   │
│  │   - "Faces" subtab       ───►  /api/people /api/faces     │   │
│  │   - person-label CRUD    ───►  /api/people/:id (open out) │   │
│  │   - watchfolder dedup    ───►  /api/watchfolders          │   │
│  │                                                            │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
                                │
                                │  HTTP (optional, when configured)
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                       CrispLens                                  │
│                                                                  │
│   v2 (Python/FastAPI):  routers/{images,faces,people,search,…}  │
│   v4 (Node/Express):    server/routes/{images,faces,people,…}.js │
│                                                                  │
│   InsightFace / dlib / YuNet / SFace face vectors                │
│   SQLite for relational state (images / albums / watchfolders)   │
└─────────────────────────────────────────────────────────────────┘
```

---

## Tier 1 — local-only Bilder tab (the FALLBACK; default for fresh installs)

This is what every user gets out of the box.  Zero external deps.
Builds entirely on existing CrispSorter infrastructure:

| Need | Already there | New code |
|------|---------------|----------|
| Image-extension dispatch | `extractors/` covers `.jpg/.png/.webp/.heic/.tiff/.bmp` | – |
| Thumbnail rendering | `image` crate is a runtime dep | thin generator helper |
| EXIF reading | `kamadak-exif` is a runtime dep | surface in row metadata |
| SHA-256 dedup | P15 batch dedup | re-use for image rows |
| LanceDB+Tantivy search | The hybrid search core | image-row filter view |
| `crisp+drive://` URIs | Drives + walker shipped 2026-05-09/10 | – |
| Folder tree, date histogram | P9 Übersicht infrastructure | reuse |
| pHash near-duplicate detection | – | NEW: `img_hash` crate, ~5 h |

**What the user sees:** new "Bilder" tab between "Übersicht" and "Archiv".
A grid of thumbnails.  A search bar that searches over:

- Filename (BM25 via Tantivy, exists)
- Embedded OCR text if the file went through OCR (exists)
- EXIF fields (camera, date, GPS-derived location) — new EXIF→searchable-string flattener
- pHash near-dup grouping toggle

Filter chips: by folder, by date range, by camera make/model (from EXIF),
by GPS country/city (reverse-geocoded offline via embedded gazetteer if
present, or just lat/lng buckets).

**Schema additions** (LanceDB only; SQLite/Tantivy unchanged):

- New columns on `doc_chunks` (nullable, only populated for image rows):
  - `phash`            `INT64`   — 64-bit perceptual hash (image-hasher's pHash)
  - `image_width`      `INT32`
  - `image_height`     `INT32`
  - `camera_make`      `STRING`  (EXIF Make)
  - `camera_model`     `STRING`  (EXIF Model)
  - `taken_at_unix`    `INT64`   — EXIF DateTimeOriginal as unix seconds
  - `gps_lat`          `FLOAT64`
  - `gps_lon`          `FLOAT64`

All nullable, `#[serde(default)]`, so existing schemas migrate
transparently (Lance adds null columns on next write).

**No new ML models in Tier 1.**

## Tier 2 — CrispLens-enhanced Bilder tab

Activated when `bilderBackend = CrispLens` in settings AND
`GET /api/health` returns 200 within the last 30 s.

### Endpoints we consume (read-only from CrispSorter)

| Verb | Path | Used for |
|------|------|----------|
| GET  | `/api/health` | 30-s polling for the Tier-2-availability banner |
| POST | `/api/auth/login` | Trade username/password for JWT (one-time) |
| GET  | `/api/auth/me` | Validate stored token on launch |
| GET  | `/api/images` | List + paginate; query params for filters |
| GET  | `/api/images/:id/thumbnail` | Grid view (cached in memory only) |
| GET  | `/api/images/:id` | Detail metadata for preview pane |
| GET  | `/api/images/:id/exif` | EXIF surface in preview pane (richer than ours) |
| GET  | `/api/images/:id/faces` | Face crops for an image |
| GET  | `/api/search?q=…` | Semantic search bar |
| POST | `/api/search/face` | "Find more pictures of this face" (drop a crop) |
| GET  | `/api/people` | Faces subtab — list of person clusters |
| GET  | `/api/people/:id` | Cluster detail (sample faces, count) |
| GET  | `/api/people/embeddings` | Vector data for offline cache (slice B-extra) |
| POST | `/api/ingest/upload-local` | Promote a `crisp+drive://` image into CrispLens |
| GET  | `/api/watchfolders` | Cross-reference with CrispSorter's `folders` list |

### Endpoints we explicitly do NOT consume

These stay in CrispLens's UI; CrispSorter shows an "↗ Open in CrispLens"
button that deep-links instead:

- All editing routes (`/api/edit/*`, `crop/adjust/convert/canvas-size`)
- BFL/FLUX generation (`/api/bfl/*`)
- Albums CRUD (`/api/albums/*`) — read-only is fine, write goes to CrispLens
- Filesystem move/copy (`/api/filesystem/move|copy`)
- Duplicates resolve actions (`/api/duplicates/resolve*`)
- All admin / users / api_keys endpoints

### Auth pattern

Mirrors what `crisp-index-server` already does:

- Settings → "Bilder backend" dropdown: `Lokal` (default) | `CrispLens`
- When `CrispLens` selected, two fields appear: `URL`, `Token (or login)`
- Login flow: button posts to `/api/auth/login` with username + password
  prompts (modal); JWT stored in **Keychain on macOS / DPAPI on Windows /
  secret-service on Linux** via tauri-plugin-stronghold (already a
  workspace dep? — verify) or `keyring-rs` (small, sync, cross-platform)
- Token expiry: on 401, re-prompt for credentials and re-issue
- Same `Authorization: Bearer <jwt>` header on every CrispLens request

### Graceful auto-degradation

Background `Bilder::HealthMonitor` task in CrispSorter (parallel to the
existing sync chip):

```text
state transitions
   None  ──(user picks CrispLens, health 200)──►  Online
   Online ──(health fails 3x in a row, ~90s)──►  Offline
   Offline ──(health 200 again)─────────────►  Online
```

When `Offline`:

- Banner appears at top of Bilder tab: "CrispLens offline — zeige lokale Sicht"
- Semantic-search input is replaced by plain text-search input (Tier 1)
- Faces subtab shows "Verbindung verloren — zuletzt N Personen" with whatever cache exists
- `Open in CrispLens` buttons grey out

When health returns:

- Banner clears
- Semantic search re-mounts (with whatever the user typed in the
  fallback bar prefilled, so they don't lose their query)
- No reload, no state loss

---

## `crisplens-protocol` crate (workspace)

New workspace member: `crates/crisplens-protocol/`.  Same role as
`crisp-index-protocol`: pure-Rust wire types shared between the
CrispSorter side and (eventually, when CrispLens has a Rust port)
the server side.

For now CrispLens speaks via FastAPI/Express; the protocol crate just
mirrors what their API returns.  Compatibility is by hand (no codegen
yet — CrispLens doesn't ship an OpenAPI spec at the moment, but
`fastapi_app.py` includes `/openapi.json` for free, so it's a follow-up
to drive type generation from there).

Initial type set:

```rust
// crisplens-protocol/src/lib.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    pub id: i64,
    pub path: String,                  // server-local path on CrispLens host
    pub filename: String,
    pub size: i64,
    pub sha256: String,
    pub phash: Option<String>,         // 64-bit pHash, hex-encoded
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub taken_at: Option<String>,      // ISO-8601 from EXIF
    pub created_at: String,
    pub modified_at: Option<String>,
    pub rating: Option<i32>,           // 0..=5
    pub flagged: Option<bool>,
    pub face_count: Option<i32>,
    pub tags: Vec<String>,
    pub exif: Option<serde_json::Value>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Face {
    pub id: i64,
    pub image_id: i64,
    pub bbox: [f32; 4],                // x, y, w, h normalised 0..=1
    pub cluster_id: Option<i64>,
    pub person_id: Option<i64>,
    pub embedding: Option<Vec<f32>>,   // 512-D ArcFace; omitted on list endpoints
    pub det_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub face_count: i32,
    pub cover_face_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagesPage {
    pub items: Vec<Image>,
    pub total: i64,
    pub page: i32,
    pub page_size: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub image: Image,
    pub score: f32,
    pub matched_field: Option<String>, // "ocr", "filename", "tags", "semantic"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,                // "ok" | "degraded"
    pub version: String,
    pub face_engine: Option<String>,   // "buffalo_l" | "dlib" | "yunet+sface"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub token_type: String,            // "bearer"
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub detail: String,
}
```

Round-trip serde tests for each type (no live server needed).

---

## Settings additions

| Setting | Default | Where stored |
|---------|---------|--------------|
| `bilderBackend` | `Local` | `tauri-plugin-store` JSON |
| `crispLensUrl` | empty | `tauri-plugin-store` JSON |
| `crispLensToken` | empty | **Keychain / DPAPI / secret-service** (NOT JSON) |
| `bilderShowFaces` | `false`, becomes `true` when Tier 2 + token valid | derived |
| `phashThreshold` | `8` (Hamming distance) | JSON |
| `bilderThumbnailSize` | `256` (px square) | JSON |

When `bilderBackend = CrispLens` but the URL is empty or unreachable,
the UI degrades to Tier 1 silently — the setting doesn't auto-revert
(so when the server comes back, we re-promote without user action).

---

## Slice breakdown with hours

| Slice | Tier | Hours | Status | Deliverable |
|-------|------|-------|--------|-------------|
| **A1** | 1 | 10 | shipped `b2853d8` | Bilder tab UI scaffold + image-row filter on existing index |
| **A2** | 1 | 6  | shipped `6795548` | Thumbnail generator (on-demand, no cache) + EXIF surfacing in preview |
| **A3** | 1 | 3  | shipped `abf7266` | SHA-256 dup view (data already in index) |
| **A4** | 1 | 6  | shipped `ce0bfbd` | `image_hasher` crate integration + near-dup grouping — chose `HashAlg::Gradient` over the spec's "DCT-pHash" wording (see [A4 deviation appendix](#a4-deviation-dct-phash--gradient-hash)) |
| **B1** | 2 | 6  | shipped `0aa3a51` | `crisplens-protocol` crate + Settings UI + Keychain session-cookie storage |
| **B2** | 2 | 5  | **scope check** | `/api/search` is **filename/person-name substring search** only (verified live + in source); spec's "semantic" wording is aspirational.  Either ship as "remote text search" or wait for upstream CrispLens to grow an embedding-based route. |
| **B3** | 2 | 8  | ready | Faces subtab + `/api/people` + `/api/images/{id}/faces` + face-crop modal.  Endpoints verified live; payload shapes captured. |
| **B4** | 2 | 4  | ready | Health monitor + degradation banner + auth refresh.  `/api/health` shape pinned in B1's `HealthResponse`. |
| **B5** | 2 | 4  | ready | Cross-link: open-in-CrispLens deep-link, watchfolder dedup signalling.  `/api/watchfolders` verified live. |
| Tests + docs | both | 6 | rolling | A1–B1 already include unit tests inline; live tests against the real server are scripted in HISTORY.md. |

**Total: ~58 h on paper.  Shipped to date (A1–A4 + B1): ~31 h.**
Tier 1 (A1–A4) is self-contained — works on a fresh install with no
external deps.  Tier 2 (B2–B5) layers on top when a CrispLens server
is configured + reachable.

---

## Risk register

| Risk | Mitigation |
|------|------------|
| Token storage — JSON config leaks credentials on backup / cloud-sync | Use Keychain / DPAPI / secret-service (`keyring-rs` crate); never write token to `tauri-plugin-store` JSON.  Settings UI only stores the URL there. |
| Wire format drift between CrispSorter's `crisplens-protocol` and the live CrispLens server | Versioned `Accept: application/json; v=1` header (CrispLens implements `/api/version` already?  Otherwise pin against a tagged CrispLens release).  CI cross-check via `CRISPLENS_TEST_URL`-gated integration test. |
| EXIF GPS leak via cloud sync | Strip `gps_lat`/`gps_lon` columns before any `SyncManager` push.  Add explicit `#[serde(skip)]` or filter in `push_pending`. |
| pHash false positives on real-world photos (e.g., bursts) | Threshold tunable in Settings; default `8` (proven safe for JPEG resizes); use 64-bit DCT-pHash for stability. |
| Tier 2 latency on slow servers | Search query timeout 5 s, falls back to Tier 1 search with banner.  Thumbnails are deferred-loaded. |
| Auth re-prompt loops if token expires mid-session | Single retry, then surface modal with "log in again" CTA. |
| User has CrispLens watching the same folder CrispSorter watches → double-ingest | When health is `Online`, compute `sha256` once and rely on whichever side saw the file first; the watchfolders endpoint cross-references this. |
| CrispLens v2 (Python) vs v4 (Node) backend feature drift | Pin against v4 (the production target per CrispLens README); document v2 compatibility as best-effort.  Health endpoint returns version + features array so we can branch on capability. |

---

## Out-of-scope (explicit, so future sessions don't drift)

- Generative image editing (`/api/edit/*`, `/api/bfl/*`) — open-in-CrispLens
- Image rotation / convert / crop performed inside CrispSorter
- Face merging / splitting UI — read-only from CrispSorter
- Writing to CrispLens (no PATCH/DELETE/POST except `/api/ingest/upload-local` and `/api/auth/login`)
- Custom CLIP model in CrispSorter — semantic search is *delegated* to CrispLens
- Re-implementing CrispLens's SQLite schema in CrispSorter's LanceDB
- Cross-clustering with face data sent over `SyncManager` — face vectors are PII; we don't sync them

---

## Implementation skeleton (signatures, no impl)

```rust
// src-tauri/src/bilder/mod.rs
//
// Bilder vertical — local-only by default, CrispLens-enhanced when configured.
// Mirrors crate::drives in shape: trait + Local impl + remote impl + registry.

pub mod local;
pub mod crisplens;
pub mod tauri_commands;

/// Source of image data + face data + semantic search.  Trait so the UI
/// switches transparently between Tier 1 (LocalBilder) and Tier 2
/// (CrispLensBilder) without knowing which is active.
pub trait BilderBackend: Send + Sync {
    /// One-shot health probe.  Used by the auto-degradation monitor.
    fn health(&self) -> Result<HealthStatus>;

    /// List image rows.  Falls back to LanceDB filter on Tier 1.
    fn list(&self, page: i32, filters: ListFilters) -> Result<ImagesPage>;

    /// Text search.  Tier 1 = Tantivy on filename+OCR+EXIF; Tier 2 = CrispLens semantic.
    fn search(&self, q: &str, limit: usize) -> Result<Vec<SearchHit>>;

    /// People clusters.  Tier 1 returns empty (no face data); Tier 2 hits /api/people.
    fn people(&self) -> Result<Vec<Person>>;

    /// Thumbnail bytes for a row.  Both tiers generate locally if the image
    /// is reachable; Tier 2 prefers CrispLens's pre-baked thumbs when available.
    fn thumbnail(&self, id: ImageRef, size: u32) -> Result<Vec<u8>>;
}

pub enum ImageRef {
    /// CrispSorter's own LanceDB doc_id
    Local(String),
    /// CrispLens's numeric image_id
    Remote(i64),
}

pub enum HealthStatus {
    Ok { version: String, face_engine: Option<String> },
    Degraded(String),
}

// 5 Tauri commands, parallel to drive_* in P11:
//   bilder_list / bilder_search / bilder_people / bilder_thumbnail / bilder_health

// New Tauri command (B1+B2):
//   bilder_login(url, username, password) -> stores token in Keychain
//   bilder_logout()                       -> wipes Keychain entry
```

---

## Implementation order (recommended)

1. **A1** — scaffold the tab, render an image-filtered grid from
   existing LanceDB.  Even a crude version that's not yet useful gives
   us the render skeleton to iterate against.
2. **A2** — thumbnails + EXIF surfacing.  Now the tab is genuinely
   useful for browsing.
3. **A3** — dup view (3 hours, easy win).
4. **A4** — pHash; the only Tier 1 feature with new dependencies.
   Validate the choice of crate (`img_hash` vs `image_hasher`) early.
5. **B1** — protocol crate, Settings UI, Keychain.  No new user-visible
   functionality yet, but Tier 2 unlocks once it lands.
6. **B2** — semantic search.  First visible Tier 2 feature.
7. **B3** — Faces subtab.  The "big" Tier 2 feature.
8. **B4** — health monitor + degradation banner.  Polish for the
   "server goes offline mid-session" case.
9. **B5** — cross-link + watchfolder dedup.  Final integration polish.

Each slice ships as one commit (or one PR if you prefer pairwise review).
`cargo install` and svelte-check should pass between every slice.

---

## How to start a fresh session for this work

```text
We're implementing P13 Bilder vertical in CrispSorter.  Plan lives at
docs/P13_Bilder_integration.md (read it end-to-end first).

The CrispLens project that Tier 2 talks to is cloned at
/Users/<user>/code/CrispLens — use its
electron-app-v4/server/routes/*.js as the authoritative endpoint
reference.

Tier 1 (A1–A4) and Tier 2 foundation (B1) are already shipped on
main.  Pick up at B2 (semantic search — but read the spec-vs-reality
appendix first; the "semantic" wording is aspirational) or B3
(Faces subtab; endpoints verified live).
```

---

## Spec vs reality appendix (2026-05-11)

The protocol-types sketch in [the `crisplens-protocol` section
above](#crisplens-protocol-crate-workspace) was written before the
live CrispLens routes were inspected.  When B1 work started against
the real source (`/Users/<user>/code/CrispLens`,
`routers/*.py` for v2 + `electron-app-v4/server/routes/*.js` for
v4) and the live production server at `https://<crisplens-host>`,
the sketch turned out to be **uniformly aspirational** — divergent
from BOTH v2 and v4 in the same direction.

| Spec claim | Reality (verified in source + live) |
|------------|--------------------------------------|
| OAuth2 bearer JWT in `Authorization` header | **httpOnly session cookie** (`session=<value>`); v2 echoes value in body, v4 cookie-only |
| `LoginResponse {access_token, token_type, expires_in}` | `{ok, username, role, token?}` |
| `Image {path, size, sha256, phash, gps_lat/lon, exif}` | `{filepath, file_size, …}`; no sha256/phash/gps/exif at list level |
| `rating: i32` | v4 emits BOTH `rating` + `star_rating`, v2 only `star_rating` (HTTP adapter renames v2→v4 before serde sees the payload) |
| `ImagesPage {items, total, page, page_size}` | v4: `{images, total}`; v2: bare array `[…]` (adapter wraps before deserialise) |
| `HealthResponse {status: "ok"\|"degraded", face_engine}` | v4: `{ok, version, backend}`; v2: `{ok, model_ready, …}` |
| `/api/search` is "semantic search" | Both v2 and v4: substring-on-filename / person-name only.  No embedding backend. |
| `Face.bbox: [x, y, w, h]` normalised | v4: `{top, right, bottom, left}` object.  v2's shape TBD at B3 time. |
| `Person {face_count, cover_face_id}` | v4: `{appearances, first_seen, last_seen, created_at}` (different fields entirely) |

**Implications baked into the codebase by B1:**

1. `crates/crisplens-protocol/` models v4-canonical names with
   permissive defaults.  16 unit tests pin both v2- and v4-shaped
   JSON payloads from the live route source.
2. The HTTP-client adapter (slice B2+) is responsible for v2→v4
   normalisation: wrap bare array in `{images, total}`, rename
   `star_rating`→`rating` and `color_flag`→`flag` before passing
   to serde.  This split keeps the protocol crate free of branchy
   version logic.
3. Auth is cookie-jar based (`reqwest`'s `cookie_store(true)`).
   Login captures the cookie from BOTH the response body (v2) and
   the `Set-Cookie` header (v4).
4. Per the spec's risk register, the cookie value lives in the OS
   keychain (`keyring` crate: Keychain on macOS, secret-service on
   Linux, Credential Manager on Windows).  The settings JSON file
   stores ONLY the URL + non-secret tunables — verified live by
   inspecting the file after a successful login.

**Future protocol-crate additions** (slices B3–B5) should be
verified against the live routes the same way — the risk-register
"wire format drift" entry is exactly this.

---

## A4 deviation: DCT-pHash → gradient hash

Spec said: "use 64-bit DCT-pHash for stability".
`image_hasher`'s `.preproc_dct()` runs the DCT on a `hash_size`-
shaped buffer rather than Krawetz's canonical "32×32 DCT →
low-frequency 8×8 block" flow.  At our wire-mandated 64-bit hash
size that means an 8×8 DCT input where the DC coefficient
dominates so heavily the per-coefficient mean threshold leaves
the hash with a single bit set.  Surfaced live during the A4 demo:
a colour gradient, an inverted gradient, AND a coarse checkerboard
fixture all hashed to identical `0x0…01`.

Workable options were:

1. Promote the wire format to 256 / 1024 bits and run DCT at
   `hash_size(16, 16)` / `(32, 32)`.  Invasive; breaks the spec's
   `phash INT64` LanceDB column promise.
2. Stay at 64 bits and switch to `HashAlg::Gradient` (compares
   adjacent pixel luminance pairs — every output bit encodes a
   directional edge).  Genuinely informative at 64 bits.

Picked option 2.  Strictly speaking that's "gHash", not pHash, but
the spec's INTENT (64-bit, robust to resize, threshold-tunable
around 8) is satisfied.  The public identifier `phash` is preserved
so the future LanceDB INT64 column lands without churn.  Top of
`src-tauri/src/images/phash.rs` carries the full rationale + the
degenerate-on-uniform-images pinning test.
