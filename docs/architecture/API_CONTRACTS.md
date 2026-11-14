# REST API Contracts (v1)

Base path: `/api/v1`. Media type: `application/json`. All timestamps RFC 3339 UTC.

---

## 1. Response envelope

Success:

```json
{
  "data": { … },
  "meta": {
    "request_id": "01JC8Q…",
    "provider": "innertube-web",
    "sources": [{"provider": "innertube-web", "endpoint": "search", "latency_ms": 184, "attempt": 0, "outcome": "ok"}],
    "cached": false,
    "fetched_at": "2026-11-13T09:31:22Z",
    "warnings": [],
    "parse_warnings": 0,
    "pagination": {"next_cursor": "eyJ…c2ln", "prev_cursor": null, "limit": 20, "returned": 20}
  }
}
```

`data` is either an object or, for list endpoints, an object with a named collection
(`{"items": [...]}`) — never a bare array, so the envelope can grow without breaking clients.

### Availability of a video document (Phase 5A)

Video documents carry `data.availability`:

| Wire form | Meaning | Served as 200? |
| --- | --- | --- |
| `{"state":"available"}` | upstream said `OK` | yes |
| `{"state":"upcoming"}` / `{"state":"live"}` | scheduled / broadcasting | yes |
| `{"state":"unavailable","reason":"<enum>"}` | upstream refused playback but returned the metadata (title, duration, channel, description) | yes — the document is real, its availability is explicit |
| `{"state":"unknown"}` | upstream answered without stating playability | yes, and `meta.parse_warnings >= 1`; never cached |

`reason` is a closed set: `deleted`, `private`, `members_only`, `login_required`, `age_restricted`,
`geo_restricted`, `not_broadcasting`, `copyright_blocked`, `drm_protected`, `other`. There is no
`unknown` reason — "we could not classify the wording" is `other`, and the raw upstream status stays in
the provider error message and the logs, never in the API response.

When upstream refuses the *metadata* as well (a bot check, a deleted id), there is no document and the
request fails with `409 content_unavailable` (§ table below), which is the only shape that carries a
reason without data.

Error (ADR-009):

```json
{
  "error": {
    "code": "invalid_cursor",
    "message": "Cursor is malformed or has expired.",
    "status": 400,
    "request_id": "01JC8Q…",
    "retryable": false,
    "details": []
  }
}
```

### Error codes (closed set, v1)

| code | HTTP | Retryable | Meaning |
|---|---|---|---|
| `invalid_request` | 400 | no | malformed query/body/param |
| `validation_failed` | 400 | no | semantic validation (with `details[]`) |
| `invalid_cursor` | 400 | no | cursor tampered/expired |
| `unauthorized` | 401 | no | missing/invalid API key |
| `forbidden` | 403 | no | key lacks scope |
| `not_found` | 404 | no | resource does not exist upstream |
| `content_unavailable` | 409 | no | deleted/private/geo-blocked/age-gated — `details[0].reason`. Emitted only when upstream returned no metadata document; when it did, the reason rides on `data.availability` instead (Phase 5A) |
| `quota_exceeded` | 429 | no (until reset) | monthly/key quota |
| `rate_limited` | 429 | yes | short-term limiter, `retry_after_ms` present |
| `provider_rate_limited` | 429 | yes | upstream rate-limited us |
| `capability_unavailable` | 503 | no | no provider implements this capability |
| `provider_unavailable` | 503 | yes | all eligible providers failed transiently |
| `provider_circuit_open` | 503 | yes | breakers open, `retry_after_ms` present |
| `timeout` | 504 | yes | deadline exceeded upstream |
| `internal_error` | 500 | maybe | unexpected; always carries `request_id` |

Rule: `retryable` is a property of the *class*, and `ContentUnavailable` is a 409 — never a 404 —
because content existence and content availability are different facts (ADR-016).

## 2. Pagination

- Query: `limit` (1..=50, default 20), `cursor` (opaque, ADR-008).
- Response: `meta.pagination.{next_cursor, prev_cursor, limit, returned}`.
- `next_cursor` is `null` when exhausted *and* the field is present, so clients can distinguish
  "no more pages" from "this endpoint does not paginate" (the latter also omits `meta.pagination`).
- Cursors are provider-bound and resource-bound; reusing a search cursor on `/comments` is a
  `400 invalid_cursor`.

## 3. Endpoint status (this repository)

Mounted only when a provider declares the capability (see `ARCHITECTURE.md` §5).

| Endpoint | Capability | Implemented | Verified |
|---|---|---|---|
| `GET /health` | — | yes | unit |
| `GET /ready` | — | yes | unit |
| `GET /status` | — | yes | unit |
| `GET /metrics` | — | yes (Prometheus text) | unit (format) |
| `GET /openapi.json`, `/docs` | — | yes (static spec) | schema validation in CI |
| `GET /search` | `SEARCH_ALL` | yes | fixture |
| `GET /search/videos` | `SEARCH_VIDEOS` | yes | fixture |
| `GET /search/channels` | `SEARCH_CHANNELS` | yes | fixture |
| `GET /search/playlists` | `SEARCH_PLAYLISTS` | yes | fixture |
| `GET /videos/{id}` | `VIDEO_METADATA` | yes | fixture + live capture (metadata fields; refusals return 409) |
| `GET /videos/{id}/metadata` | `VIDEO_METADATA` | yes | fixture |
| `GET /videos/{id}/related` | `VIDEO_RELATED` | yes | fixture |
| `GET /videos/{id}/chapters` | `VIDEO_CHAPTERS` | yes | fixture |
| `GET /channels/{id}` | `CHANNEL_METADATA` | yes | fixture |
| `GET /channels/{id}/about` | `CHANNEL_ABOUT` | yes | fixture |
| `GET /channels/{id}/videos` | `CHANNEL_VIDEOS` | yes | fixture |
| `GET /channels/{id}/playlists` | `CHANNEL_PLAYLISTS` | yes | fixture |
| `GET /playlists/{id}` | `PLAYLIST_METADATA` | yes | fixture |
| `GET /playlists/{id}/items` | `PLAYLIST_ITEMS` | yes | fixture |
| `GET /videos/{id}/captions` | `CAPTIONS_METADATA` | yes | fixture |
| `GET /videos/{id}/comments` | `COMMENTS_THREAD` | no (Phase 6) | — |
| `GET /comments/{id}/replies` | `COMMENTS_REPLIES` | no (Phase 6) | — |
| `GET /live`, `/live/{id}` | `LIVE_METADATA` | no (Phase 7) | — |
| `GET /trending`, `/categories`, `/recommendations` | `TRENDING`/… | no (Phase 7) | — |
| `GET /search/music` | `SEARCH_MUSIC` | no (Phase 7) | — |

"Verified" means: covered by an offline test in this repository. Live upstream verification is
**not** claimed anywhere (see `docs/PROGRESS.md`).

## 4. Identifier validation

| Kind | Rule | Rejection |
|---|---|---|
| video id | `^[A-Za-z0-9_-]{11}$` | `400 validation_failed` |
| channel id | `^UC[A-Za-z0-9_-]{22}$` or `@handle` (resolved) | `400 validation_failed` |
| playlist id | `^(PL|UU|LL|OL|RD|FL)[A-Za-z0-9_-]{10,40}$` | `400 validation_failed` |
| query | trimmed, 1..=200 chars, no control chars | `400 validation_failed` |
| locale | BCP-47 subset, `hl` + `gl` | ignored if unsupported (never 400) |

Validation happens in `vydra-common::ids` (newtypes) so an invalid id can never reach a provider and
cannot be used to build an arbitrary upstream URL (SSRF hardening).

## 5. Standard query parameters

| Param | Applies to | Default | Notes |
|---|---|---|---|
| `hl` | all extraction | `en` | BCP-47 language subtag, affects both text and numbers |
| `gl` | all extraction | `US` | ISO-3166-1 alpha-2 content country |
| `limit`, `cursor` | list endpoints | 20 | see §2 |
| `sort` | search, comments | `relevance` | `relevance\|date\|views\|rating` / `top\|new` |
| `filter` | search | — | repeatable: `video,channel,playlist,live,hd,4k,subtitles,creative_commons,3d,360,vr180,hdr` |
| `since` | search | — | `hour\|day\|week\|month\|year` |
| `duration` | search | — | `short\|medium\|long` |
| `fields` | all | — | sparse fieldset (comma list); unknown fields → `400 validation_failed` |

## 6. Rate limiting headers

Every response includes:

```
X-Request-Id: 01JC8Q…
X-RateLimit-Limit: 60
X-RateLimit-Remaining: 41
X-RateLimit-Reset: 1763024482
Retry-After: 2            (only on 429)
```

## 7. Versioning

`/api/v1` is frozen per contract. Additive changes (new fields, new enum variants in `warnings`) are
non-breaking; removing or renaming requires `/api/v2`. Deprecations are announced via the
`Sunset` header on affected endpoints and listed in `docs/api/CHANGELOG.md`.
