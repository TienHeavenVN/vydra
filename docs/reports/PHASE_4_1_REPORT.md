# Phase 4.1 — Provider Engine Audit and Compile Stabilisation

**Branch**: `arena/06c8b7fc-vydra` (branched from `630437c` on `main`)
**Scope**: the existing Provider Engine only — `vydra-providers` (transport, InnerTube client,
parser toolkit), `vydra-provider-registry`, `vydra-engines`, the HTTP edge in `vydra-api`,
`vydra-observability`, and the configuration that drives them. No new features, no redesign.
**Date**: 2026-11-13

---

## 1. Files inspected

| Area | Files |
| --- | --- |
| Workspace | `Cargo.toml`, `deny.toml`, `Dockerfile`, `docker-compose.yml`, `.github/workflows/ci.yml`, `scripts/ci/*.sh` |
| Providers | `crates/vydra-providers/src/{lib,parse,transport}.rs`, `src/innertube/{mod,client,map,params,provider}.rs`, `tests/search_fixture.rs`, `tests/fixtures/*` |
| Registry | `crates/vydra-provider-registry/src/lib.rs` (breaker, windows, gates, fallback ordering) |
| Engines | `crates/vydra-engines/src/lib.rs` (search/video/chapters/captions, caching, cursors) |
| Domain / common | `crates/vydra-domain/src/{common,video,search,channel}.rs`, `crates/vydra-common/src/{ids,text,cursor}.rs` |
| Edge | `crates/vydra-api/src/lib.rs`, `crates/vydra-api/src/*` middleware |
| Support | `crates/vydra-cache`, `crates/vydra-auth`, `crates/vydra-config`, `crates/vydra-errors`, `crates/vydra-observability`, `crates/vydra-database`, `apps/*` |
| Docs | `docs/PROGRESS.md`, `docs/security/PROVIDER_ENGINE_AUDIT.md`, `docs/architecture/PROVIDER_ENGINE.md`, `docs/research/IMPLEMENTATION_DECISIONS.md`, `docs/api/openapi.yaml` |

Every Rust file in the repository (32 files) was also parsed with tree-sitter, because the sandbox
has no compiler. That check catches syntax errors only — it cannot resolve names or types, and it
missed every `E02xx`/`E0432` that CI later reported.

## 2. Confirmed defects

Phase 4.1 closed twenty findings (A-01…A-20, catalogued in `docs/security/PROVIDER_ENGINE_AUDIT.md`).
Compile stabilisation then closed seven more, each with a failing CI run as evidence:

| ID | Sev | Defect | Evidence |
| --- | --- | --- | --- |
| A-16 | S1 | `thiserror` (config) and `serde_json` (engines) used but undeclared — workspace did not compile | run 43420474500 `E0433` |
| A-17…A-20 | S1–S3 | clippy `-D warnings` failure, Dockerfile `COPY Cargo.lock`, audit/deny jobs unable to run, OpenAPI without a security scheme | runs 43419351202 / 43420474500 |
| A-21 | S1 | **Cache could never hit**: fields that skip serialization when unset required `#[serde(default)]` on read, so `from_str::<VideoDocument>` failed with `missing field published_at` and every entry was re-fetched | run 43432463005 |
| A-22 | S1 | **No channel handle could parse**: `ChannelHandle` validated the whole string against the bare-id charset, which excludes `@` | run 43430663406 |
| A-23 | S2 | **Scraped API key always discarded**: `find_api_key` required `len() > 20`, rejecting the shape it was documented to find | runs 43444806970, 43445452262 |
| A-24 | S2 | **Search results in reverse document order**: LIFO stack pushed in source order | run 43446155668 |
| A-25 | S2 | Layout-only renderers (`sectionListRenderer`, …) counted as *unknown content* | run 43447524439 |
| A-26 | S2 | A test fixture doubled per iteration (2²⁰⁰ nodes) and hung the test job for 20 minutes with no output | run 43439144664 |
| A-27 | S3 | Unused crate/workspace dependencies and missing `[lints] workspace = true` failed the build under `-D warnings` | runs 027d39b…8296c26 |

## 3. Fixes applied

* **Domain serialization (A-21)** — `#[serde(default)]` on all 56 `Option`/`Vec` fields that carry
  `skip_serializing_if`, including the two multi-line `time::serde::rfc3339::option` attributes;
  the caching test now asserts a serde round-trip, not just a cache hit.
* **Id validation (A-22)** — handle validation strips the `@` sigil before charset checks and allows
  `.` in the remainder (1–39 characters), matching YouTube handle rules.
* **Session bootstrap (A-23)** — one shared predicate, `AIza` + 12..=60 URL-safe characters, used by
  both the `sw.js_data` and watch-page paths.
* **Parser ordering (A-24)** — children pushed in reverse so the LIFO walk yields pre-order in
  document order.
* **Parse diagnostics (A-25)** — `STRUCTURAL_RENDERERS` documents the containers that are traversed
  but not counted as unknown content.
* **Fixture (A-26)** — linear 200-deep chain (still 4× `MAX_DEPTH`) replaces the exponential one;
  the fixture's 25-character channel id became a valid 24-character one.
* **Manifests (A-27)** — unused dependencies removed at crate and workspace level; both apps inherit
  the workspace lint set; `workspace.package` fields no member inherits removed.
* **MSRV (RUSTSEC-2026-0009)** — `rust-version = "1.88"` and `time = "0.3.55"`: every fixed `time`
  release requires 1.88, so the 1.85 floor was pinning a vulnerable version.
* **CI observability** — see §5.

## 4. Tests added

The suite (125 test functions) was written in Phase 4 and is only now executed end to end; this
phase added the assertion that closes A-21 and the harness that makes a hanging test visible.

| Package | Tests | What they pin down |
| --- | --- | --- |
| `vydra-providers` | 46 | transport validation (scheme, userinfo, port, fragment, header count, body/response caps, IP literals), replay transport, session bootstrap + single-flight, key/visitor discovery, error classification, deadline handling, parser renderers/text/continuations, budgets (depth/nodes), search + player mapping, fixture-driven search |
| `vydra-common` | 26 | id newtypes (video/channel/handle/playlist), injection rejection, count/duration parsing, signed cursors |
| `vydra-provider-registry` | 12 | capability selection, sliding window, breaker transitions incl. half-open probe success/failure, concurrent failures, rate/concurrency gating, fallback ordering and attempt bounds, "content facts never fall back" |
| `vydra-engines` | 6 | cache hit/miss/freshness, locale-scoped keys, cursor minting + tamper rejection, type-scoped filtering, provenance/sources |
| `vydra-api`, `vydra-config`, `vydra-auth`, `vydra-cache`, `vydra-errors`, `vydra-domain`, `vydra-observability` | 35 | envelope + error mapping, config guardrails/redaction, key hashing and quotas, cache contract, error taxonomy and fallback eligibility, model serialization |
| `vydra-database`, `apps/*` | 0 | no unit tests; the schema is covered by migrations and the Docker build only |

Added in this phase: the serde round-trip assertion in `video_metadata_is_cached_and_flagged`, and
`scripts/ci/test-package.sh` (per-package cap plus an opt-in per-test probe that names a hang).

## 5. Exact commands executed

Everything that compiles ran on GitHub Actions; nothing was compiled in the sandbox.

```bash
# CI (each job is a separate runner)
cargo fmt --all -- --check
cargo check --workspace --all-targets --message-format=short
cargo clippy --workspace --all-targets --all-features --message-format=short -- -D warnings
cargo test --package <each of the 14 members> --all-features --message-format=short
cargo generate-lockfile && cargo audit --ignore RUSTSEC-2024-0437 --ignore RUSTSEC-2026-0459 --ignore RUSTSEC-2023-0071
cargo deny check
gitleaks detect
npx @redocly/cli lint docs/api/openapi.yaml
docker build -t vydra:ci .

# Local (sandbox) checks — advisory only
python3 -c "import tomllib; tomllib.loads(open('Cargo.toml').read())"   # every manifest
node /tmp/parsecheck/parse.cjs .                                        # tree-sitter, 32 files
bash -n scripts/ci/*.sh
npx @redocly/cli lint docs/api/openapi.yaml
```

## 6. Actual results

* `fmt`, `check`, `clippy (‑D warnings)`, `audit`, `deny`, `secrets`, `openapi`, `docker`: **success**.
* `test`: **14/14 packages success** (one job per package).
* Total: **22 of 22 jobs green**; run duration ≈ 11 minutes.
* Local: 32/32 Rust files parse; all manifests load; all CI scripts pass `bash -n`; OpenAPI lints
  clean. These are *not* substitutes for compilation and are not reported as such.
* Eighteen pushes were required. Each failing run named a real defect — no failure was a CI quirk
  except the three observability problems described in ADR-017, which were fixed in the workflow.

## 7. CI run URL / status

**Green**: commit `8f82286` (head of `arena/06c8b7fc-vydra`), run **43450513810** —
<https://github.com/TienHeavenVN/vydra/actions/runs/43450513810> — status `completed`, conclusion
`success`, 22/22 jobs. The two previous heads (`68d1416` run 43449726141, `19d504e` run 43448947831)
were also fully green.
First fully green run of the suite: commit `524b0d7`, run **43448221721** (23 jobs, including the
temporary `fmt-apply` job that has since been retired).

## 8. Remaining blockers

1. **No local toolchain.** Every compile is a nine-to-fourteen minute round trip. The sandbox cannot
   install Rust (`static.rust-lang.org` unreachable) and cannot reach `crates.io`.
2. **No live upstream verification.** `www.youtube.com` is unreachable and anti-bot circumvention is
   out of scope (ADR-006), so InnerTube request shapes are derived from the reference implementations
   in `docs/research/`, not observed. Experimental capabilities stay disabled.
3. **Fixtures are synthetic** (`synthetic_schema_reference`). Recording real payloads needs network
   access and a licensing decision.
4. **`Cargo.lock` is not committed** (ADR-019). CI and Docker resolve it per run; committing it needs
   one real `cargo generate-lockfile`.
5. **DNS-rebinding hardening** needs a resolver hook (design work, not debugging); the allowlist plus
   redirect-free client closes the easy paths but not a poisoned resolution.
6. **Parser budget sanity is code-level only** — that deep/large payloads terminate is asserted by
   unit tests, not by fuzzing.

## 9. Security risks

| Risk | State |
| --- | --- |
| SSRF via allowlist bypass (userinfo, non-443, IP literals, redirects) | closed and regression-tested; no redirect following at all, `https_only(true)` |
| DNS rebinding past the allowlist | **open by design**: documented in `PROVIDER_ENGINE_AUDIT.md` §4, needs a resolver hook |
| Credential leakage (hardcoded key, logs, error bodies) | closed: A-01, redaction in `vydra-config`, gitleaks in CI, no key in any log path |
| Cache poisoning across locales/providers | closed: `hl`/`gl` in the key, config-hash invalidation, failures never cached |
| Unbounded response/body/decompression | closed: 256 KiB request cap, 4 MiB response cap enforced while streaming, no decompression beyond what `reqwest` negotiates |
| Breaker black-holing a healthy provider | closed (A-12/A-13), half-open recovery tested |
| Quota/DoS from anonymous callers | closed: per-key and per-IP buckets, per-provider token bucket + semaphore |
| Anti-bot circumvention (sig/n-sig/PoToken) | deliberately absent; experimental capabilities default off |
| Supply chain (unmaintained/advisory crates) | `cargo audit` + `cargo deny` green with four documented, reasoned ignores |

## 10. Next recommended phase

**Phase 4.2 — Provider contract verification against recorded fixtures**, before any new engine:
capture a small, legally usable set of search/player responses, promote them to `recorded` fixtures,
and re-run the parser suite against them. The engine is now compile- and test-clean, but its request
shapes are still unverified against the live service — that gap, not new features, is what stands
between "green" and "trustworthy". Feature work (channels, playlists, related) should wait until the
fixture set proves the existing three endpoints end to end.
