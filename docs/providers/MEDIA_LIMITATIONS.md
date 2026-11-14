# Media playback limitations (ADR-006)

Vydra is a **data** API. It does not return playable media URLs that require defeating an integrity or
anti-bot control.

## What YouTube protects

1. `signatureCipher` — the `s` parameter must be transformed by a function inside the player
   JavaScript (`MediaServiceCore/.../innertube/core/Player.kt`, `NewPipeExtractor/.../YoutubeSignatureUtils.java`).
2. `n`-parameter throttling — a JS transformation of the `n` query parameter; a wrong value yields
   throttled delivery (`NewPipeExtractor/.../YoutubeThrottlingParameterUtils.java`).
3. `pot` (proof-of-origin) — a BotGuard-derived token appended to media URLs
   (`MediaServiceCore/.../PoTokenGate`, `Piped-Backend/utils/BgPoTokenProvider.java`,
   `yt-dlp/yt_dlp/extractor/youtube/pot/`).

All three require executing Google's obfuscated JavaScript / VM inside an environment whose integrity
is being measured. That is circumvention of an access control, so Vydra does not implement it.

## What Vydra serves instead

- `formats[]` with `itag`, `mime`, `codecs`, bitrate, dimensions, fps, content length, audio track,
  DRM flag, and `delivery` (`direct | signature_protected | token_protected | drm | unknown`).
- `availability` (`available | upcoming | live | unavailable(reason)`), so a client can distinguish
  "private", "age restricted", "geo blocked" and "deleted" without guessing.
- No `url` field exists on the domain type, so a future contributor cannot accidentally add one
  without changing the model and the ADR.

## If a reviewer wants playback anyway

The only architecture Vydra would accept is the one Invidious uses: an **external, separately
deployed companion** behind a capability trait (`TranscriptCapable`-style), never inline in a
provider, and never enabled by default. That decision is explicitly out of scope for this repository
and would need legal review first.
