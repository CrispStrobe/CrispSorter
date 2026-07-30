#!/usr/bin/env python
"""Distribute an uploaded build to TestFlight via the App Store Connect API.

Everything here is API-doable — no browser, no Xcode GUI. The steps follow
`../appstore.md` Step 9, which was confirmed end-to-end on other apps in
this account.

Two modes, because they are genuinely different processes:

* **internal** — any existing App Store Connect team member, no Apple
  review, live within minutes of processing.
* **external** — needs a review contact, a beta description in the app's
  **primary** locale, per-build "what to test" notes, and a Beta App Review
  submission. Review is much lighter than full App Store review but is not
  instant.

Both are idempotent: a group that exists is reused, a localization that
409s is PATCHed instead. Safe to re-run on the same build.

Usage (env-driven so CI passes secrets, never argv):

    ASC_KEY_ID=... ASC_ISSUER_ID=... ASC_KEY_P8_BASE64=... ASC_APP_ID=... \\
    python scripts/testflight_distribute.py --platform IOS --mode internal

    ... --platform MAC_OS --mode external --public-link

`--dry-run` prints what it would do and touches nothing.
"""
import argparse
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"

# Contact + copy for external review.
#
# These are the same details the app publishes in its own Impressum
# (Settings → Legal → Contact, `src/lib/components/Settings.svelte`), which
# makes them the single source of truth: a reviewer who compares the review
# contact against what the app displays sees the same person and number.
# Nothing here is secret — it is published in-app and legally required in
# Germany — so it lives in the script rather than in repo secrets, and the
# env vars are for overriding per-app, not for hiding anything.
REVIEW_CONTACT = {
    "contactFirstName": os.environ.get("ASC_CONTACT_FIRST", "Christian"),
    "contactLastName": os.environ.get("ASC_CONTACT_LAST", "Ströbele"),
    "contactEmail": os.environ.get("ASC_CONTACT_EMAIL", "postmaster@crispstro.be"),
    "contactPhone": os.environ.get("ASC_CONTACT_PHONE", "+49 176 6421 8601"),
    "demoAccountRequired": False,
    "notes": (
        "CrispSorter is an offline-first document organizer. All AI processing "
        "runs on-device. No account or login required. Import any PDF, DOCX, or "
        "image to test search, OCR, and batch rename features."
    ),
}
BETA_DESCRIPTION = (
    "CrispSorter sorts documents into a clean folder hierarchy using a local "
    "LLM, and indexes them for offline full-text and semantic search. Nothing "
    "leaves the device unless you configure a cloud provider yourself."
)


def log(msg):
    print(msg, flush=True)


# ── Auth ────────────────────────────────────────────────────────────────
def make_token(key_id: str, issuer_id: str, p8: bytes) -> str:
    """ES256 JWT for the App Store Connect API.

    `exp` is 15 minutes, not the maximum 20: Apple rejects anything over 20
    and has been observed to reject exactly-20 depending on clock skew
    between the runner and their edge.

    Everything about the inputs is logged except the inputs themselves —
    lengths, PEM header, and the decoded JWT header/claims. A 401 from Apple
    says only "provide a properly configured and signed bearer token", which
    is indistinguishable between a wrong key id, an empty issuer, a p8 that
    decoded to garbage, and a malformed claim set; this narrows it without
    printing a private key into a public log.
    """
    try:
        import jwt  # PyJWT
    except ImportError:
        sys.exit("need PyJWT with crypto: pip install 'pyjwt[crypto]'")

    pem = p8.decode() if isinstance(p8, bytes) else p8
    first = pem.strip().splitlines()[0] if pem.strip() else "<empty>"
    log(f"   key id: {len(key_id)} chars ({key_id[:4]}…)  "
        f"issuer: {len(issuer_id)} chars  p8: {len(pem)} chars, starts {first!r}")
    if "BEGIN" not in first:
        sys.exit("the decoded key is not a PEM — check that the secret holds "
                 "base64 of the .p8 file, not the raw file or a path")

    now = int(time.time())
    claims = {"iss": issuer_id, "iat": now, "exp": now + 15 * 60,
              "aud": "appstoreconnect-v1"}
    token = jwt.encode(claims, pem, algorithm="ES256",
                       headers={"kid": key_id, "typ": "JWT"})
    # Echo back what we actually signed, decoded from the token itself rather
    # than from the variables we think we passed.
    try:
        import base64 as _b64, json as _json
        h, c, _ = token.split(".")
        pad = lambda x: x + "=" * (-len(x) % 4)
        log(f"   jwt header: {_json.loads(_b64.urlsafe_b64decode(pad(h)))}")
        decoded = _json.loads(_b64.urlsafe_b64decode(pad(c)))
        log(f"   jwt claims: iss={decoded['iss'][:8]}… aud={decoded['aud']} "
            f"ttl={decoded['exp'] - decoded['iat']}s")
    except Exception as e:  # never fail the run over diagnostics
        log(f"   (could not decode our own token for logging: {e})")
    return token


class Asc:
    def __init__(self, token, dry_run=False):
        self.token = token
        self.dry_run = dry_run

    def _req(self, method, path, body=None):
        url = path if path.startswith("http") else f"{API}{path}"
        if self.dry_run and method != "GET":
            log(f"   [dry-run] {method} {url}")
            if body:
                log(f"   [dry-run] {json.dumps(body)[:200]}")
            return {}, 200
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.token}")
        if data:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=90) as r:
                raw = r.read()
                return (json.loads(raw) if raw else {}), r.status
        except urllib.error.HTTPError as e:
            raw = e.read().decode(errors="replace")
            try:
                parsed = json.loads(raw)
            except json.JSONDecodeError:
                parsed = {"raw": raw}
            return parsed, e.code

    def get(self, path):
        return self._req("GET", path)

    def post(self, path, body):
        return self._req("POST", path, body)

    def patch(self, path, body):
        return self._req("PATCH", path, body)


def errors_of(payload):
    return "; ".join(
        f"{e.get('title')}: {e.get('detail')}" for e in payload.get("errors", [])
    ) or json.dumps(payload)[:300]


# ── Steps ───────────────────────────────────────────────────────────────
def latest_build(asc, app_id, platform, version=None, wait_minutes=25):
    """The newest build for this app+platform, waited until it is VALID.

    Apple processes an upload asynchronously; a build in PROCESSING cannot be
    assigned to a group, so waiting is part of the job rather than something
    the caller should have to know.
    """
    q = urllib.parse.urlencode({
        "filter[app]": app_id,
        "filter[preReleaseVersion.platform]": platform,
        "sort": "-version",
        "limit": "5",
        "fields[builds]": "version,processingState,uploadedDate,usesNonExemptEncryption",
    })
    deadline = time.time() + wait_minutes * 60
    seen = None
    while True:
        payload, status = asc.get(f"/builds?{q}")
        if status == 401:
            sys.exit(
                "App Store Connect rejected the token (401). The key itself is "
                "probably fine — altool in the preceding step uses the same "
                "secret — so suspect the claim set or the key/issuer pairing. "
                "Compare the jwt header and claims logged above against the "
                "key in App Store Connect → Users and Access → Integrations: "
                f"kid must be that key's ID and iss its issuer ID. Apple said: "
                f"{errors_of(payload)}"
            )
        if status != 200:
            sys.exit(f"listing builds failed ({status}): {errors_of(payload)}")
        builds = payload.get("data", [])
        if version:
            builds = [b for b in builds if b["attributes"].get("version") == version]
        if builds:
            b = builds[0]
            state = b["attributes"].get("processingState")
            seen = f"{b['attributes'].get('version')} ({state})"
            if state == "VALID":
                log(f"   build {b['id']} version {b['attributes'].get('version')} is VALID")
                return b
            if state in ("INVALID", "FAILED"):
                sys.exit(f"build {b['id']} processing state is {state} — nothing to distribute")
        if time.time() > deadline:
            sys.exit(
                f"no VALID build within {wait_minutes} min (last seen: {seen or 'none'}). "
                "Apple is still processing, or the upload never arrived."
            )
        log(f"   waiting for processing… (last seen: {seen or 'none'})")
        time.sleep(60)


def set_export_compliance(asc, build):
    """`usesNonExemptEncryption` gates *all* testing.

    A build uploaded without `ITSAppUsesNonExemptEncryption` in Info.plist
    comes out of processing with this null, and TestFlight then offers it to
    nobody — internal or external — with no obvious error anywhere.
    """
    if build["attributes"].get("usesNonExemptEncryption") is not None:
        log("   export compliance already answered")
        return
    payload, status = asc.patch(
        f"/builds/{build['id']}",
        {"data": {"type": "builds", "id": build["id"],
                  "attributes": {"usesNonExemptEncryption": False}}},
    )
    if status not in (200, 204):
        sys.exit(f"setting export compliance failed ({status}): {errors_of(payload)}")
    log("   export compliance set (exempt: standard HTTPS/TLS only)")


def ensure_group(asc, app_id, name, internal):
    """Find the beta group by name, or create it. Idempotent."""
    q = urllib.parse.urlencode({"filter[app]": app_id, "limit": "50"})
    payload, status = asc.get(f"/betaGroups?{q}")
    if status == 200:
        for g in payload.get("data", []):
            if g["attributes"].get("name") == name:
                log(f"   reusing group '{name}' ({g['id']})")
                return g["id"]
    body = {"data": {"type": "betaGroups",
                     "attributes": {"name": name, "isInternalGroup": internal},
                     "relationships": {"app": {"data": {"type": "apps", "id": app_id}}}}}
    payload, status = asc.post("/betaGroups", body)
    if status in (200, 201):
        gid = payload.get("data", {}).get("id", "<dry-run>")
        log(f"   created {'internal' if internal else 'external'} group '{name}' ({gid})")
        return gid
    sys.exit(f"creating group '{name}' failed ({status}): {errors_of(payload)}")


def assign_build(asc, group_id, build_id):
    payload, status = asc.post(
        f"/betaGroups/{group_id}/relationships/builds",
        {"data": [{"type": "builds", "id": build_id}]},
    )
    # 409 = already assigned, which is a success for our purposes.
    if status in (200, 201, 204):
        log("   build assigned to the group")
    elif status == 409:
        log("   build was already assigned to the group")
    else:
        sys.exit(f"assigning build failed ({status}): {errors_of(payload)}")


def enable_public_link(asc, group_id, limit=None):
    """Turn on the group's public TestFlight link and return it.

    This is how external testers join here: a URL anyone can open, rather
    than per-email invitations. Apple only issues the link for an *external*
    group, and only once Beta App Review has passed — before that the field
    comes back null, which is not an error, just "not yet".
    """
    attrs = {"publicLinkEnabled": True}
    if limit is not None:
        attrs["publicLinkLimitEnabled"] = True
        attrs["publicLinkLimit"] = int(limit)
    payload, status = asc.patch(
        f"/betaGroups/{group_id}",
        {"data": {"type": "betaGroups", "id": group_id, "attributes": attrs}},
    )
    if status != 200:
        log(f"   WARNING: enabling the public link failed ({status}): {errors_of(payload)}")
        return None
    link = payload.get("data", {}).get("attributes", {}).get("publicLink")
    if link:
        log(f"   public TestFlight link: {link}")
    else:
        log("   public link enabled; Apple issues the URL once Beta App Review passes")
    return link


def add_testers(asc, group_id, emails):
    for email in emails:
        first, _, last = email.partition("@")
        body = {"data": {"type": "betaTesters",
                         "attributes": {"email": email, "firstName": first[:30] or "Tester",
                                        "lastName": last.split(".")[0][:30] or "Tester"},
                         "relationships": {"betaGroups": {
                             "data": [{"type": "betaGroups", "id": group_id}]}}}}
        payload, status = asc.post("/betaTesters", body)
        if status in (200, 201):
            log(f"   invited {email}")
        elif status == 409:
            log(f"   {email} already a tester")
        else:
            log(f"   WARNING: inviting {email} failed ({status}): {errors_of(payload)}")


def primary_locale(asc, app_id):
    payload, status = asc.get(f"/apps/{app_id}?fields[apps]=primaryLocale")
    if status == 200:
        return payload.get("data", {}).get("attributes", {}).get("primaryLocale") or "en-US"
    return "en-US"


def ensure_beta_app_localization(asc, app_id, locale, description, feedback_email):
    """Beta description, in the locale Beta App Review actually checks.

    The gotcha from appstore.md: `POST /betaAppReviewSubmissions` 422s with
    "betaAppLocalizations not found for this app" when the description is
    missing in the app's *primary* locale — an en-US-only one is not enough.
    """
    q = urllib.parse.urlencode({"filter[app]": app_id, "limit": "50"})
    payload, status = asc.get(f"/betaAppLocalizations?{q}")
    existing = {}
    if status == 200:
        existing = {d["attributes"].get("locale"): d["id"] for d in payload.get("data", [])}
    attrs = {"description": description, "feedbackEmail": feedback_email}
    if locale in existing:
        payload, status = asc.patch(
            f"/betaAppLocalizations/{existing[locale]}",
            {"data": {"type": "betaAppLocalizations", "id": existing[locale],
                      "attributes": attrs}})
        log(f"   updated beta description for {locale}"
            if status == 200 else
            f"   WARNING: updating {locale} description failed ({status}): {errors_of(payload)}")
        return
    body = {"data": {"type": "betaAppLocalizations",
                     "attributes": {"locale": locale, **attrs},
                     "relationships": {"app": {"data": {"type": "apps", "id": app_id}}}}}
    payload, status = asc.post("/betaAppLocalizations", body)
    if status in (200, 201):
        log(f"   created beta description for {locale}")
    elif status == 409:
        log(f"   beta description for {locale} already exists")
    else:
        sys.exit(f"creating beta description for {locale} failed ({status}): {errors_of(payload)}")


def ensure_review_details(asc, app_id):
    """The review contact resource auto-exists per app; its id == the app id."""
    payload, status = asc.patch(
        f"/betaAppReviewDetails/{app_id}",
        {"data": {"type": "betaAppReviewDetails", "id": app_id,
                  "attributes": REVIEW_CONTACT}})
    if status == 200:
        log("   review contact set")
    else:
        sys.exit(f"setting review contact failed ({status}): {errors_of(payload)}")


def ensure_whats_new(asc, build_id, locale, notes):
    q = urllib.parse.urlencode({"filter[build]": build_id, "limit": "50"})
    payload, status = asc.get(f"/betaBuildLocalizations?{q}")
    existing = {}
    if status == 200:
        existing = {d["attributes"].get("locale"): d["id"] for d in payload.get("data", [])}
    if locale in existing:
        asc.patch(f"/betaBuildLocalizations/{existing[locale]}",
                  {"data": {"type": "betaBuildLocalizations", "id": existing[locale],
                            "attributes": {"whatsNew": notes}}})
        log(f"   updated 'what to test' for {locale}")
        return
    body = {"data": {"type": "betaBuildLocalizations",
                     "attributes": {"locale": locale, "whatsNew": notes},
                     "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}}
    payload, status = asc.post("/betaBuildLocalizations", body)
    if status in (200, 201, 409):
        log(f"   'what to test' set for {locale}")
    else:
        log(f"   WARNING: 'what to test' for {locale} failed ({status}): {errors_of(payload)}")


def submit_for_beta_review(asc, build_id):
    """The step that actually unlocks external installs."""
    payload, status = asc.post(
        "/betaAppReviewSubmissions",
        {"data": {"type": "betaAppReviewSubmissions",
                  "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
    if status in (200, 201):
        state = payload.get("data", {}).get("attributes", {}).get("betaReviewState", "?")
        log(f"   submitted for Beta App Review — state: {state}")
    elif status == 409:
        log("   already submitted for Beta App Review")
    else:
        sys.exit(f"beta review submission failed ({status}): {errors_of(payload)}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--platform", required=True, choices=["IOS", "MAC_OS"])
    ap.add_argument("--mode", required=True, choices=["internal", "external"])
    ap.add_argument("--group", default=None, help="group name (default depends on mode)")
    ap.add_argument("--testers", default="",
                    help="comma-separated emails (internal groups; external "
                         "testing here uses a public link instead)")
    ap.add_argument("--public-link", action="store_true",
                    help="enable the group's public TestFlight URL (external only)")
    ap.add_argument("--public-link-limit", type=int, default=None,
                    help="cap the number of testers who can join via the link")
    ap.add_argument("--version", default=None, help="build version to target (default: newest)")
    ap.add_argument("--notes", default=None, help="'what to test' text")
    ap.add_argument("--wait-minutes", type=int, default=25)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    key_id = os.environ.get("ASC_KEY_ID")
    issuer = os.environ.get("ASC_ISSUER_ID")
    app_id = os.environ.get("ASC_APP_ID")
    p8_b64 = os.environ.get("ASC_KEY_P8_BASE64")
    p8_path = os.environ.get("ASC_KEY_P8_PATH")
    if not all([key_id, issuer, app_id]) or not (p8_b64 or p8_path):
        sys.exit("need ASC_KEY_ID, ASC_ISSUER_ID, ASC_APP_ID and "
                 "ASC_KEY_P8_BASE64 (or ASC_KEY_P8_PATH)")
    p8 = base64.b64decode(p8_b64) if p8_b64 else open(p8_path, "rb").read()

    asc = Asc(make_token(key_id, issuer, p8), dry_run=args.dry_run)
    group = args.group or ("Internal Testers" if args.mode == "internal"
                           else "External Testers")
    notes = args.notes or (
        "Automated TestFlight build from CI. Try: import a folder of PDFs, "
        "run a search, and check the PDF editor (arrange / crop / redact)."
    )

    log(f"TestFlight: platform={args.platform} mode={args.mode} group='{group}'"
        + (" [dry-run]" if args.dry_run else ""))

    log("1. locating the build")
    build = latest_build(asc, app_id, args.platform, args.version, args.wait_minutes)
    build_id = build["id"]

    log("2. export compliance")
    set_export_compliance(asc, build)

    log("3. beta group")
    gid = ensure_group(asc, app_id, group, internal=(args.mode == "internal"))
    assign_build(asc, gid, build_id)

    emails = [e.strip() for e in args.testers.split(",") if e.strip()]
    if emails:
        log("4. testers")
        add_testers(asc, gid, emails)

    if args.mode == "external":
        log("5. external prerequisites")
        loc = primary_locale(asc, app_id)
        feedback = REVIEW_CONTACT["contactEmail"]
        # Primary locale first — that is the one review checks — then en-US
        # if the app's primary is something else, so the listing reads well
        # for English testers too.
        ensure_beta_app_localization(asc, app_id, loc, BETA_DESCRIPTION, feedback)
        if loc != "en-US":
            ensure_beta_app_localization(asc, app_id, "en-US", BETA_DESCRIPTION, feedback)
        ensure_review_details(asc, app_id)
        ensure_whats_new(asc, build_id, loc, notes)
        log("6. beta app review")
        submit_for_beta_review(asc, build_id)
        if args.public_link:
            log("7. public link")
            enable_public_link(asc, gid, args.public_link_limit)
        log("\nExternal testing is pending Apple's Beta App Review "
            "(usually well under a day). The public link starts working once "
            "review passes; until then it 404s for testers.")
    else:
        log("\nInternal testing is live — testers must be App Store Connect "
            "team members; `GET /v1/users` lists who is eligible.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
