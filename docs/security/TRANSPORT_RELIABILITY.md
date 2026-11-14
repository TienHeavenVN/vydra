# Transport reliability (Phase 4.3)

How Vydra's outbound path behaves when things go wrong, what is proven by tests, and what is
deliberately refused. Scope: `crates/vydra-providers/src/transport.rs` and the InnerTube client on top
of it. Nothing here weakens the DNS/host boundary described in
`docs/security/DNS_REBINDING_ANALYSIS.md`; where a property cannot be tested from CI it is written
down as untested instead of being claimed.

## 1. The boundary, as configured

`ReqwestTransport::new()` (`transport.rs:230`) builds one `reqwest::Client` with:

| Setting | Value | Why |
|---|---|---|
| `https_only(true)` | — | a plaintext hop can be rewritten by anything on the path (`transport.rs:232`) |
| `redirect(Policy::none())` | refused, returned as a status | a 3xx is the only place a *server* could move the request to a host Vydra never validated (`transport.rs:246`) |
| `dns_resolver(GuardedResolver)` | per-answer filtering | a hostname allowlist alone loses to DNS rebinding (`transport.rs:249`) |
| `no_proxy()` | proxies refused | a proxy resolves the target itself, so the guard would no longer cover the hop that matters (`transport.rs:242`) |
| `pool_max_idle_per_host(16)`, `pool_idle_timeout(90s)`, `tcp_keepalive(60s)` | — | bounded reuse; see §4 |
| `connect_timeout(3s)` | — | connect stalls fail fast instead of eating the request budget |

On every request, before any DNS work, `TransportRequest::validate()` (`transport.rs:55`) rejects:
non-`https` schemes, userinfo (`https://www.youtube.com@evil.example/`), ports other than 443,
fragments, hosts outside the allowlist (`youtube.com`, `www/music/m.`, `youtubei.googleapis.com`,
`i.ytimg.com`, `yt3.ggpht.com`, `yt3.googleusercontent.com` and their `*.` suffixes), header names
outside `[A-Za-z0-9_-]`, CR/LF in header values, more than 32 headers, and bodies over 256 KiB.

At resolution time `GuardedResolver` (`transport.rs:486`) refuses a non-allowlisted name *before*
resolving, then hands every answer to `guard_resolution` (`transport.rs:337`), which:

* drops each address whose IP is unspecified, loopback, private, link-local, broadcast, multicast,
  CGNAT, documentation, benchmarking or reserved space (IPv4-mapped and IPv4-compatible IPv6 are
  judged as the IPv4 address they embed);
* keeps the permitted addresses, sorts and de-duplicates them, and returns exactly those — so the
  connection uses an address that was checked, with no second lookup in between;
* **fails closed** when nothing is permitted (`BlockedUrl: resolution yielded no permitted address`).

Because the permitted set is what gets connected to, a dual answer (`142.250.72.14` + `169.254.169.254`)
connects only to the public address; it does not fall back to the private one.

## 2. Failure paths and the codes the caller sees

| Transport event | `TransportError` | Provider error / `code()` | Test |
|---|---|---|---|
| context deadline already passed | — (no request issued) | `Timeout` → `provider_timeout` | `an_expired_deadline_never_reaches_the_network` |
| caller's timeout expires while in flight | future dropped | caller sees its own timeout | `a_cancelled_future_drops_the_in_flight_request` |
| per-request timeout | `Timeout(d)` | `Timeout` → `provider_timeout` | `a_transport_timeout_is_reported_as_a_provider_timeout` |
| body over `max_response_bytes` (4 MiB default) | `BodyTooLarge { limit }` | `Parse` → `provider_parse` | `an_oversized_body_is_reported_as_a_parse_error_naming_the_cap` |
| connection refused/reset/DNS failure | `Connect(_)` | `Transport` → `provider_transport` | `a_connection_failure_is_wrapped_and_not_retried` |
| 3xx answer | none (it is a response) | `Upstream { status }` → `provider_upstream` | `a_redirect_is_surfaced_and_never_followed` |
| 401/403 | none | `Unauthorized` (session invalidated, re-bootstrapped once) | covered by request-contract tests |
| 429 | none | `RateLimited { retry_after }` | covered by `retry_after_is_parsed` |

Tests are in `crates/vydra-providers/tests/transport_reliability.rs`; the mapping itself is
`map_transport_error` (`innertube/client.rs:357`).

Two properties of that table are worth stating explicitly:

* **No retries happen inside the transport or the provider.** One `fetch` is at most one bootstrap
  plus one endpoint call; backoff and circuit breaking belong to the registry. The
  `a_connection_failure_is_wrapped_and_not_retried` test asserts the call count, so an accidental
  retry loop shows up as a failing test rather than as extra upstream traffic.
* **Timeout is bounded twice.** `InnertubeClient::timeout_for` (`client.rs:178`) uses
  `min(config.request_timeout (6 s), ctx.remaining())`, and `Transport::execute` wraps the whole
  exchange in `tokio::time::timeout` (`transport.rs:318`). A context with no time left is rejected
  before a request is constructed.

### Cancellation

Dropping the `Provider::fetch` future drops the in-flight `reqwest` call; nothing keeps running in the
background and no background task holds the response. The test drives a provider whose transport
sleeps for 30 s, wraps the call in a 50 ms timeout, and asserts both that it returns and that the
request had already been issued (`calls == 2`: bootstrap + search). This is what a request-scoped
timeout in the API layer relies on.

## 3. Proxies: refused on purpose

`no_proxy()` is not a defensive default that someone forgot to make configurable — it is the decision
recorded for audit finding A.30. `reqwest` honours `HTTPS_PROXY`/`https_proxy` from the environment,
and a CONNECT proxy performs its own DNS resolution; the request would then reach an address
`GuardedResolver` never saw, defeating the guard on exactly the hop it was written for. Since there is
no API to configure a proxy, the property is enforced *by construction*: a deployment that needs an
egress proxy must (a) add a `Transport` implementation whose proxy is pinned by configuration and
(b) accept that the guard now applies to the proxy hop, not the target. That trade-off is written
down in `docs/security/DNS_REBINDING_ANALYSIS.md` §5.

There is no test for "the environment proxy is ignored": from a scripted transport the environment is
not observable, and an integration test would have to reach the network to prove it. What CI does
prove is that the only constructor (`ReqwestTransport::new`) builds the client with `no_proxy()` on the
path that every production request takes.

## 4. Pooled connections and the DNS guard — the honest version

`reqwest` pools established connections per `(scheme, host, port)` and **reuses them without resolving
again**. So:

* the resolver guard runs when a connection is *created*, not on every request;
* a name that resolved to a permitted address at T0 keeps talking to that address at T1 even if the
  DNS answer changed — which is exactly what makes rebinding hard, but also means a *stale* answer is
  reused for up to `pool_idle_timeout` (90 s);
* a connection whose peer later becomes internal (address reassignment, not rebinding) would be reused
  as-is. TLS hostname verification is unaffected (the URL still carries the hostname and the
  certificate is still checked against it), so the peer must still present a valid certificate for a
  YouTube host.

Consequences taken seriously in the design: idle connections are dropped after 90 s, keepalive probes
stop at 60 s, redirects are refused so a pooled connection can never be re-aimed by a server, and the
allowlist has no host that is under an operator's control (unlike, say, a CDN hostname that a customer
chooses). What is **not** mitigated: an address that becomes internal *between* two requests on the
same pooled connection. That requires an attacker who controls the peer's public address space, and it
ends when the pool entry expires; it is recorded here rather than pretended away.

Not tested, by nature: connection reuse itself, DNS TTL behaviour, and rebinding against a live
resolver. CI has no egress to `youtube.com`, and a test that mocked the resolver would only assert its
own mock. The resolver's *policy* is unit-tested (`guard_resolution_*`,
`resolver_refuses_a_host_that_is_not_allowlisted_without_resolving_it`), and the live behaviour is
covered indirectly: the five Phase 4.3 captures went through this exact client, with the guard active,
from a GitHub runner (see `docs/research/SEARCH_RESPONSE_ANALYSIS.md`).

## 5. What the live captures add

The probe (`crates/vydra-providers/examples/probe_search.rs`) is the only code path that talks to the
real endpoint, and it goes through `ReqwestTransport` unchanged — no test double, no relaxed
configuration:

* 5 requests, statuses 200/200/200/200/400, bodies 664 211 / 263 144 / 1 396 201 / 21 748 / 274 bytes,
  elapsed 638 / 320 / 688 / 1 328 / 18 ms (run `43542839419`);
* the 400 is a typed error document, not an HTML page and not a redirect, so the malformed-body path
  is exercised end to end;
* the bootstrap (`sw.js_data`) produced a visitor id and an API key on every run, so session handling
  works against the live host as well.

## 6. Residual risks

1. **Pinned addresses under a pooled connection** — §4. Accepted, bounded by the idle timeout.
2. **No proxy support at all** — a deployment behind a mandatory egress proxy cannot use the provider
   as-is; §3 explains what implementing it would cost.
3. **Certificate validation depends on the CA bundle** — standard `rustls`/`webpki` roots; a
   compromised root is out of scope.
4. **Body cap is per response, not per session** — five 1 MB pages are 5 MB of memory in flight; the
   cap protects one exchange, not aggregate traffic.
5. **`Retry-After` is parsed but not honoured here** — scheduling is the registry's job; the provider
   simply reports `provider_rate_limited` with the hint attached.
