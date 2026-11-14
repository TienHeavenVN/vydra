# Provider Engine — architecture and extension guide

Status: **Phase 4.1** (independent audit + compile stabilisation). This document describes what the
code *is*, not what it aspires to be. Anything unverified is labelled as such.

---

## 1. Component map

```
HTTP (vydra-api)
  │  search / video / chapters / captions handlers, envelope, error mapping,
  │  auth + per-key/per-IP quota, request-id, timeout, body limit
  ▼
Engines (vydra-engines)
  │  use cases, cache policy, cursor minting/validation, capability gate for
  │  unverified filters, provenance assembly (incl. meta.sources)
  ▼
Registry (vydra-provider-registry)
  │  capability selection, health window, circuit breaker, per-provider
  │  semaphore + token bucket, bounded fallback, attempt recording, observer hook
  ▼
Provider contract (vydra-providers)
  │  ProviderRequest/ProviderResponse (typed), CapabilitySet, ProviderContext,
  │  transport abstraction (ReqwestTransport | ReplayTransport)
  ▼
InnerTube provider (vydra-providers::innertube)
     client profiles + session bootstrap, params builder, JSON → domain mapping,
     parse report (warnings, never panics)
```

Cross-cutting: `vydra-domain` (the public value objects, including `Provenance` and
`SourceAttempt`), `vydra-errors` (the single error taxonomy), `vydra-config` (env surface),
`vydra-observability` (tracing + Prometheus).

**Dependency direction is one-way.** `vydra-api` knows HTTP; nothing below it imports `axum`.
`vydra-providers` never imports the registry or the engines. Adding a provider therefore touches one
crate plus one line in `apps/api-server/src/main.rs` — handlers, engines and the domain model are
untouched. That property is the reason the contract exists at all.

---

## 2. Request lifecycle (search)

1. **Edge** parses and validates the query (`q` length, control chars, `hl`/`gl` shape, typed
   `sort/since/duration/filter`). Unknown values are `400`; filters are refused with
   `capability_unavailable` unless `VYDRA__PROVIDERS__ENABLE_UNVERIFIED_FILTERS=true`.
2. **Engine** decodes the signed cursor (kind-scoped, HMAC), builds the cache key
   (`kind:query:filters:cursor:limit:hl:gl`), and returns a cache hit with `cached=true` and an empty
   `sources` list.
3. **Engine** builds a `ProviderContext` (absolute deadline `call_budget`, request id, locale,
   strictness) and asks the registry.
4. **Registry** filters providers by declared capability, then by breaker state (consuming at most
   one half-open probe), then by token bucket, then acquires a concurrency permit with
   `try_acquire_owned` (never queues). Candidates are tried in registration order, capped by
   `max_attempts`.
5. Each attempt is wrapped in the caller's remaining deadline. Success records health, emits an
   `AttemptOutcome::Ok|Empty` entry and returns. A health-affecting failure records health, emits
   `AttemptOutcome::Failed`, and either falls through (transient) or returns immediately
   (deterministic content fact).
6. **Engine** filters mixed payloads to the requested `SearchKind`, replaces the provider cursor with
   a signed one, attaches the attempt list to `provenance.sources`, caches (search only when
   configured) and returns.
7. **Edge** wraps `data` + `meta` (`request_id`, `provider`, `sources`, `cached`, `fetched_at`,
   `parse_warnings`, `pagination`).

---

## 3. The provider contract

```rust
#[async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    fn id(&self) -> ProviderId;                       // stable, unique, allowlisted charset
    fn capabilities(&self) -> CapabilitySet;           // what this provider actually implements
    fn client_label(&self) -> String;                  // e.g. "web-1", shown in /status only
    async fn fetch(&self, request: ProviderRequest, ctx: &ProviderContext)
        -> Result<ProviderResponse, ProviderError>;
}
```

Rules that the type system does *not* enforce, and that reviews must:

* `fetch` must honour `ctx.deadline` (the registry also wraps the call, belt and braces).
* `fetch` must be cancellation-safe: dropping the future must not corrupt provider state.
* `fetch` must never return `Unavailable`/`NotFound` for a *transient* condition: those two errors
  are content facts and suppress fallback (ADR-007).
* `capabilities()` must under-report rather than over-report. A capability declared but not
  implemented turns into a 500; a capability implemented but not declared merely hides an endpoint.

### Error taxonomy (`ProviderError` → public `Error`)

| ProviderError | Fallback? | Counts against health? | Public code |
| --- | --- | --- | --- |
| `Unavailable { reason, .. }` | **no** | no | `content_unavailable` (409) |
| `NotFound { .. }` | **no** | no | `not_found` (404) |
| `Unsupported { .. }` | no | no | `capability_unavailable` (501) |
| `InvalidRequest { .. }` | no | no | `validation_failed` (400) |
| `CircuitOpen { .. }` | n/a | no | `provider_circuit_open` (503) |
| `RateLimited { .. }` | yes | yes | `provider_rate_limited` (503/429) |
| `Unauthorized { .. }` | yes | yes | `provider_unavailable` (503) |
| `Upstream { status, .. }` | yes | yes | `provider_unavailable` (503) |
| `Timeout` | yes | yes | `timeout` (504) |
| `Transport { .. }` | yes | yes | `provider_unavailable` (503) |
| `Parse { .. }` | yes | yes | `provider_unavailable` (503) |

---

## 4. Registry algorithm

```
candidates(capability):
  for provider in registration_order:
      skip if !capability declared
      skip if breaker says no (open, or half-open with a probe already in flight)
      skip if token bucket empty        -> counted as "throttled"
      keep
fetch():
  if no provider declares the capability: 501 capability_unavailable
  if candidates empty:
      throttled > 0 ? 429 rate_limited : 503 provider_circuit_open
  for provider in candidates.take(max_attempts):
      try_acquire permit (fail fast -> Skipped + rate_limited)
      run with remaining deadline
      ok      -> record ok,     return response + attempts
      failure -> record (if affects_health), append attempt
                 !is_fallback_eligible ? return immediately : remember and continue
  return last transient error
```

Breaker state machine (deterministic, clock-injected via `Instant::now` in production and tests):

```
Closed --(window >= min_samples && success% < threshold)--> Open{until = now + cooldown}
Open   --(now >= until)-------------------------------------> HalfOpen (probe slot free)
HalfOpen --(probe ok)---------------------------------------> Closed (cooldown counter reset)
HalfOpen --(probe fails)------------------------------------> Open (cooldown *= 2, capped)
```

`cooldown = base * 2^min(consecutive_opens, 6)` capped at `max_cooldown`.

---

## 5. Caching

| Class | Key | TTL | Cleared by |
| --- | --- | --- | --- |
| `Search` | kind, query, filters, cursor token, limit, `hl`, `gl` | `cache.search_ttl_secs` (default 60s) | TTL, provider-set fingerprint (config hash) |
| `Metadata` | video id, `hl`, `gl` | `cache.metadata_ttl_secs` (default 300s) | TTL, fingerprint |
| `Session` (provider-internal) | client profile | `session_ttl_secs` | TTL |
| Negative results | — | **never cached** | n/a |

Cache hits set `provenance.cached = true` and clear `provenance.sources` (the attempts describe the
fetch that filled the cache, not this request).

Two invariants keep the cache honest:

* **Everything cached is JSON, so everything cached must round-trip.** Any domain field that skips
  serialization when unset carries `#[serde(default)]`; without it the reader rejects the writer's
  own output (`missing field published_at`) and every lookup misses while the entry is still stored.
  `video_metadata_is_cached_and_flagged` asserts the round-trip, not just the hit.
* **Unavailable content is never cached.** Availability moves in both directions (private → public,
  upcoming → available), so a negative document is never a durable fact.

---

## 6. Configuration surface (provider engine only)

| Variable | Default | Meaning |
| --- | --- | --- |
| `VYDRA__PROVIDERS__MAX_ATTEMPTS` | `2` (clamped 1–3) | provider calls per request |
| `VYDRA__PROVIDERS__DEFAULT_HL` | `en` | content language |
| `VYDRA__PROVIDERS__DEFAULT_GL` | `US` | content country |
| `VYDRA__PROVIDERS__ENABLE_UNVERIFIED_FILTERS` | `false` | expose `sort/since/duration/filter` and the type-scoped search capabilities |
| `VYDRA__PROVIDERS__INNERTUBE__ENABLED` | `true` | mount the InnerTube provider |
| `VYDRA__PROVIDERS__INNERTUBE__DEFAULT_CLIENT` | `web` | `web`, `mweb` or `tv` |
| `VYDRA__PROVIDERS__INNERTUBE__REQUEST_TIMEOUT_MS` | see `vydra-config` | per-call timeout |
| `VYDRA__PROVIDERS__INNERTUBE__MAX_RESPONSE_BYTES` | ≤ 4 MiB | hard response cap |
| `VYDRA__PROVIDERS__INNERTUBE__API_KEY` | **unset** | operator-supplied key; without it the key is discovered during session bootstrap |

No credential is compiled into the binary (ADR-018).

---

## 6a. Parser ordering and diagnostics

* **Document order is preserved.** `collect_renderers` walks breadth-first in document order
  (children are pushed onto the stack in reverse so the LIFO pop restores source order). A search
  response whose first item is a video yields a video first; anything else is a bug, because the
  caller sees a reordered result list and cursor pagination stops being append-only.
* **Three counters, three meanings** (Phase 4.3, from the live captures):
  * `unknown_renderer` — a renderer key the mapper does not know. On a real first page this is 800+
    occurrences of *chrome* (dialogs, filter chips, menus, share panels); it is the inventory of UI
    shapes, not of lost content.
  * `unmapped_result` — a content shape that was recognised as a *result* and dropped: the Shorts
    shelf items (`shortsLockupViewModel`), the `lockupViewModel` cards, any `KNOWN_RENDERERS` entry
    without a mapping arm. This is the number that means "the caller sees fewer results than the
    page contains", and it is what `content_problems()` sums.
  * `skipped_item` / `missing_field` / `structural` — the item was there but unusable.
  `KNOWN_RENDERERS` are mapped; `STRUCTURAL_RENDERERS` (`sectionListRenderer`, `itemSectionRenderer`,
  `continuationItemRenderer`, …) are traversed but never counted, because they carry structure or a
  token and nothing else. `UNMAPPED_CONTENT_VIEW_MODELS` is the explicit list of content shapes that
  are *known to be content* and *known to be unmapped* — the three counters only stay honest if that
  list is kept short and is emptied by mapping, not by deleting entries.
* **Strict mode is a fixture tool, not a runtime mode.** It fails on any counter, and a live page
  always has chrome, so `Strict` is used against reduced documents (the fixtures). Making it fail on
  `content_problems()` instead is a deliberate Phase 4.4 decision (see `parse.rs::check`).
* **Budgets.** `MAX_DEPTH = 48`, `MAX_NODES = 100_000`, `MAX_WARNINGS = 24`. Exceeding them emits a
  warning and stops descending; it never truncates silently.

## 6.1 Request contracts and transport security (Phase 4.2)

What Vydra sends is pinned by `tests/request_contract.rs` and documented, with per-field citations, in
`docs/research/INNERTUBE_REQUEST_CONTRACTS.md`: endpoint URLs (`?prettyPrint=false&key=…`), headers,
the shared `context` object, and the exact body keys of search, continuation and player requests. The
api key is a query parameter obtained from session bootstrap — never a header, never a literal, never
logged.

The transport boundary resolves the target *before* the socket exists (`GuardedResolver`), refuses
prohibited IPv4/IPv6 ranges (including IPv4 addresses wrapped in IPv6), fails closed when nothing
survives, refuses redirects and refuses ambient proxies. The analysis, the address tables and the
residual risks are in `docs/security/DNS_REBINDING_ANALYSIS.md`; what the transport does when a
request times out, is cancelled, hits the body cap or receives a 3xx — and what a *pooled* connection
means for the resolver guard — is in `docs/security/TRANSPORT_RELIABILITY.md` (Phase 4.3).

Bodies are structs now (`innertube/request.rs`), so "a continuation cannot carry a query" is a
compile-time property, not a convention.

## 7. Adding a provider (the whole checklist)

1. Implement `Provider` in a new module of `vydra-providers` (or a new crate depending only on
   `vydra-providers`, `vydra-domain`, `vydra-errors`).
2. Declare only capabilities you can prove with fixtures; everything else stays undeclared so the
   endpoint is not mounted.
3. Talk to the network exclusively through the `Transport` trait, so tests can use `ReplayTransport`.
4. Add fixtures under `tests/fixtures/{synthetic,recorded,malformed}/` plus a `.meta.json` sidecar
   describing provenance and a `not_verified` list — see `docs/research/FIXTURE_PROVENANCE.md`.
   Vydra has never captured a fixture itself; a recording must come from a project whose licence
   permits reuse, and it must never be described as live verification.
5. Register it in `apps/api-server/src/main.rs` (`providers.push(Arc::new(...))`). Nothing else
   changes: no handler, no engine, no domain type.
6. Add a registry test only if the provider has unusual failure semantics; the shared contract tests
   already cover fallback, breaker and gating behaviour.

---

## 8. What is deliberately absent

* No sig/n-sig/PoToken solving, no player-JS execution, no download/stream URLs, no transcripts.
* No writes of any kind to upstream services: the engine is read-only.
* No crawling, no aggressive retry loops, no parallel request fan-out beyond `max_attempts`.
* No silent degradation: every stub, synthetic fixture and unverified capability is labelled in
  `docs/PROGRESS.md` and in this file.
