# DNS rebinding: analysis and the transport boundary that closes it

Audit item **A-28** (opened in `PROVIDER_ENGINE_AUDIT.md` as S1, "DNS rebinding open") and **A-30**
(proxy bypass, found while writing this analysis). Status: both fixed in code; the residual risks that
cannot be fixed in code are listed in §6.

---

## 1. Threat model

Vydra accepts URLs from configuration and from its own endpoint templates, and it validates them
against a host allowlist (`transport.rs::is_allowed_host`: exact hosts `youtube.com`,
`www.youtube.com`, `m.youtube.com`, `music.youtube.com`, `www.googleapis.com`,
`youtubei.googleapis.com`, `i.ytimg.com`, `yt3.ggpht.com`, `yt3.googleusercontent.com`, plus the
suffixes `.youtube.com`, `.ytimg.com`, `.ggpht.com`, `.googleusercontent.com`). The suffix entries are
the interesting ones for this analysis: they are wildcards, so the set of names that can reach
`connect(2)` is large and only partly under our control. Everything before A-28 was enforced on the
**string**, and a string is not an address.

The attacker model that matters:

1. **A name on the allowlist resolves to an address inside the deployment's trust boundary.** The
   realistic paths are compromised or poisoned DNS for an allowlisted name, a CNAME into a zone the
   attacker controls, or a future allowlist entry (self-hosted Piped/Invidious instances are the
   obvious candidates for Phase 5). The attacker does not need to control YouTube; they only need to
   control one answer for one name once.
2. **Two-answer rebinding.** The name resolves to a public address when the guard runs, then to
   `127.0.0.1` / `169.254.169.254` / an RFC 1918 address when the connection is opened. This is what
   "validate, then connect" gets wrong by default: validation and connection perform *separate*
   lookups, and the second one is never checked.
3. **Redirect-mediated bypass.** The allowlist host answers `302 Location: http://127.0.0.1:8080/`
   and the client follows it, never re-validating the new URL.

What the attacker gains in each case: Vydra is a request *signer* of sorts — it sends a fixed
`content-type: application/json` body and a small header set, and returns the response body to the
caller through `/search` or `/videos/{id}/metadata`. Reaching an internal HTTP service with a
controlled method and body, and reading its response (up to `max_response_bytes`, which is 8 MiB by
default), is a usable SSRF primitive even when the body is JSON.

## 2. What the old boundary did and did not do

| Control | Before | Reaches the socket? |
| --- | --- | --- |
| Scheme must be `https` | yes | — |
| Host must be on the allowlist | yes | ✗ (string only) |
| No userinfo, no non-443 port, no fragment | yes | ✗ |
| Header shape / count / body size limits | yes | — |
| Redirects followed | **no** (`Policy::none()`) | — |
| Resolved address checked | **no** | — |
| Proxy honoured from the environment | **yes** (`reqwest` default) | — |

So two of the three attack paths above were open: (1) any bad answer for an allowlisted name went
straight to `connect(2)`, and (2) `HTTPS_PROXY` could move the connection somewhere the guard would
never look. Redirects were already structurally impossible.

## 3. The fix: a resolver, not a check

`reqwest` exposes exactly one hook that runs between name resolution and socket creation:

```
pub trait Resolve: Send + Sync {
    fn resolve(&self, name: Name) -> Resolving;   // Resolving = Pin<Box<dyn Future<Output = Result<Addrs>>>>
}
type Addrs = Box<dyn Iterator<Item = SocketAddr> + Send>;
```

Evidence: `https://github.com/seanmonstar/reqwest/blob/v0.12.12/src/dns/resolve.rs` and
`ClientBuilder::dns_resolver` in `src/async_impl/client.rs` (same tag). Two properties make this the
right hook:

* It is called **once per connection attempt**, and the connector dials *the addresses the future
  returns*. There is no second lookup between validation and `connect`, so the two-answer attack has
  nothing to exploit: the address that was validated is the address that is dialled.
* It leaves TLS alone. The URL still carries the hostname, so SNI and certificate verification still
  apply — an IP-level guard does not weaken hostname verification, and hostname verification does not
  cover the IP selection problem. They are complementary.

### Implementation

```rust
// transport.rs — ReqwestTransport::new()
.dns_resolver(Arc::new(GuardedResolver::new()))
.no_proxy()                       // A-30
.redirect(reqwest::redirect::Policy::none())
.https_only(true)
```

```rust
impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        // 1. re-check the host allowlist (defence in depth: the URL gate already ran)
        // 2. resolve with tokio::net::lookup_host((host, 443))
        // 3. guard_resolution(addrs): drop every prohibited address, fail closed if none survive
        // 4. hand the surviving SocketAddrs to the connector
    }
}
```

`guard_resolution` returns `TransportError::BlockedUrl` when every address is prohibited, so an
attacker who wins the DNS race does not get a connection attempt with an empty address list — the
request fails before any socket exists.

## 4. Address policy

`prohibited_ip_reason(ip) -> Option<&'static str>` is the single policy function; the reason string is
logged with the address so a blocked resolution is diagnosable.

**IPv4**

| Requirement | Rejected |
| --- | --- |
| unspecified | `0.0.0.0/8` (`is_unspecified` + first octet 0) |
| loopback | `127.0.0.0/8` |
| private | `10/8`, `172.16/12`, `192.168/16` |
| link-local | `169.254/16` (cloud metadata lives here) |
| broadcast / multicast | `255.255.255.255`, `224/4` |
| carrier-grade NAT (RFC 6598) | `100.64/10` |
| documentation (RFC 5737) | `192.0.2/24`, `198.51.100/24`, `203.0.113/24` |
| benchmarking / reserved | `198.18/15`, `240/4` |

**IPv6**

| Requirement | Rejected |
| --- | --- |
| unspecified / loopback / multicast | `::`, `::1`, `ff00::/8` |
| unique-local | `fc00::/7` |
| link-local | `fe80::/10` |
| documentation | `2001:db8::/32` |
| tunnelled / blackhole | Teredo `2001::/32`, discard `100::/64` |
| **anything wrapping IPv4** | `::ffff:a.b.c.d` (IPv4-mapped), `::a.b.c.d` (IPv4-compatible), `2002::/16` (6to4) — judged by the **embedded** address, then rejected by name |

The wrapping cases are the ones a naive `Ipv6Addr::is_loopback()` check misses: `::ffff:127.0.0.1` is
not `is_loopback()`, and `::ffff:169.254.169.254` is a metadata endpoint wearing an IPv6 costume.

## 5. Why the connection is bound to the verified resolution

* The guard returns the *same* `SocketAddr` values it validated; `reqwest` connects to those, not to a
  re-resolved name (evidence: `Resolve`/`Addrs` above; "an explicitly specified port in the URL
  overrides the port in the resolved `SocketAddr`", which is why the code resolves with port 443 —
  the transport already rejects any other port).
* Redirects are not followed, so no second hop exists to re-validate.
* Proxies are refused (`no_proxy()`), so the connection cannot be delegated to an entity that
  resolves the name itself. This is a deliberate trade: a deployment that needs an egress proxy must
  configure it consciously, and then accept that §6 item 3 applies.
* TLS verification is untouched: rustls validates the certificate chain against the **hostname**, and
  the guard never changes the URL.

## 6. Residual risks (not fixable here, and stated rather than hidden)

1. **Connection reuse.** A pooled connection is reused without a new resolution. This is not a bypass
   — the reused socket was validated when it was opened — but it means a blocked-address event is
   observed only when a new connection is made. Idle pool: 16 per host, 90 s.
2. **A mutually-trusted host.** If an allowlisted host genuinely serves an attacker's content over a
   valid certificate, the guard says nothing: it constrains *where* Vydra connects, not *what* the
   peer returns. Response-side trust is a separate problem (parse warnings, no execution of returned
   data, no URL following).
3. **Explicit proxy configuration.** `no_proxy()` removes the ambient `HTTPS_PROXY` bypass. If a
   future deployment adds a proxy on purpose, the guard no longer covers the hop to the origin and
   this document must be amended with the proxy's own controls.
4. **System resolver behaviour.** `lookup_host` honours `/etc/hosts` and any local resolver; a
   compromised *local* resolver is out of scope — but even then the returned address must survive the
   policy, which is the point of validating at the socket boundary.
5. **Address ordering.** Surviving addresses are sorted before being returned, which makes behaviour
   deterministic for tests but discards the resolver's preference order (Happy Eyeballs will still
   work; it is just not preferred by the OS). The trade is deliberate: a nondeterministic order makes
   the security tests flaky.
6. **`ReplayTransport` is not guarded.** It never touches the network (by construction), so tests that
   use it neither exercise nor weaken the guard.

## 7. Tests (deterministic, no network access)

| Test | Asserts |
| --- | --- |
| `prohibited_ipv4_ranges_are_rejected` | every IPv4 row of §4, including metadata `169.254.169.254` |
| `public_ipv4_addresses_are_permitted` | `142.250.72.14`, `8.8.8.8`, `1.1.1.1` pass |
| `prohibited_ipv6_ranges_are_rejected` | unique-local, link-local, documentation, Teredo, `::ffff:127.0.0.1`, `::ffff:169.254.169.254`, `::1`, `2002:7f00:1::` |
| `public_ipv6_addresses_are_permitted` | `2607:f8b0::/32`-style global unicast passes |
| `guard_resolution_drops_blocked_addresses_and_keeps_order_independence` | mixed answer keeps only the public address, identical regardless of input order |
| `guard_resolution_fails_closed_when_every_address_is_blocked` | all-blocked ⇒ `BlockedUrl`, no partial list |
| `resolver_refuses_a_host_that_is_not_allowlisted_without_resolving_it` | the resolver, called with `rebind.example`, errors before any lookup |
| `resolver_blocks_a_hostname_that_resolves_to_loopback` | `localhost` is refused by the resolver even though it is a real name |
| `validation_blocks_plain_http_and_foreign_hosts`, `rejects_ip_literals_and_userinfo`, `rejects_non_default_ports_and_fragments`, `rejects_other_schemes`, `rejects_oversized_request_body_and_header_count` | the URL/header layer that runs before resolution |

None of these tests resolve a real name or open a socket: the address tables are pure functions and
the resolver tests use names that fail the allowlist. The two resolver tests that do call
`tokio::net::lookup_host` (`localhost`) are safe because the allowlist gate fires first — if that gate
ever regressed, the test would resolve a local name, which is exactly what it is meant to catch.

## 8. Verifying the claim in this document

* `prohibited_ip_reason` is public (`pub fn`), so a reviewer can call it directly.
* `GuardedResolver` is public and implements `reqwest::dns::Resolve`; it is installed in
  `ReqwestTransport::new()` and cannot be bypassed by a caller of that type.
* `Transport::execute` calls `TransportRequest::validate()` **before** handing the request to
  `reqwest`, so the string-level checks still run for every request, including ones constructed by
  tests.
* This analysis was written from the code as it exists on branch `arena/06c8b7fc-vydra` after the A-28
  and A-30 fixes; line-level citations are avoided on purpose because the file moves. The function and
  type names above are stable.
