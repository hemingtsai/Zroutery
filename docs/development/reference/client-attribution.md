# Client Attribution Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

Client attribution answers "which application sent this?" for reporting. It is
not the routing identity: routing decides which upstream identity a request
should present, and a request routed as `native` because nothing was detected is
not evidence that the caller is "not Codex".

## The ladder

`identify(headers, binding)` is the only implementation, shared by every ingress
(`crates/zroutery-core/src/client.rs::identify`). It runs in this order:

1. a validated named gateway credential — established by authentication;
2. `x-zroutery-client`, if it names a client the gateway knows;
3. a verified client-specific header pair (`Codex`, `Claude Code`, `OpenCode`);
4. an exact `User-Agent` prefix or canonical bare name (library, SDK and HTTP
   families are `Weak` here);
5. an unregistered but real `User-Agent`, kept as a weak sanitised custom
   bucket;
6. a held declaration at `Weak`, otherwise `unknown`.

A declaration that does not match a known client is held to the end, so a typo
cannot outrank a rule that would have answered correctly.

## Evidence and confidence

`ClientFingerprint` carries the answer and its evidence
(`crates/zroutery-core/src/client.rs::ClientFingerprint`):

- `client_id`, `client_name`, `family`;
- `source`: which ladder rung matched
  (`crates/zroutery-core/src/client.rs::AttributionSource`);
- `confidence`: `Strong`, `Probable`, `Weak` or `Unknown`
  (`crates/zroutery-core/src/client.rs::Confidence`).

Only `CredentialBinding` is `Strong`: it follows authentication and cannot be
forged by a header. `User-Agent` and explicit headers are caller-controlled, so
they are at most `Probable`. A generic runtime or SDK match is `Weak`. The
confidence helper `is_recognised` excludes `Weak`; a `python-requests` match
identifies a library, not an application
(`crates/zroutery-core/src/client.rs::Confidence::is_recognised`).

`ClientFamily` is a display and aggregation axis, not a detection input:
`cli`, `ide`, `desktop`, `sdk`, `agent_framework`, `http`, `unknown`
(`crates/zroutery-core/src/client.rs::ClientFamily`).

## Canonical ids

A client id is never a free-form echo of a header
(`crates/zroutery-core/src/client.rs::ClientId`):

- The sanitising constructor lowercases, replaces characters outside `[a-z0-9._-]`,
  collapses separator runs, truncates at 64 characters, and folds an empty
  result to `unknown` (`crates/zroutery-core/src/client.rs::ClientId::sanitised`);
- the cap is `MAX_CUSTOM_CLIENT_ID_LEN` (`crates/zroutery-core/src/client.rs::MAX_CUSTOM_CLIENT_ID_LEN`);
- `ClientRegistry` keeps at most 256 distinct ids; beyond that new ids share
  `custom-other`, a count rather than a dropped request
  (`crates/zroutery-core/src/client.rs::ClientRegistry`,
  `crates/zroutery-core/src/client.rs::MAX_TRACKED_CLIENTS`).

Collapsing and truncation are deliberate: a caller varying punctuation or case
per request must not get a new aggregation dimension for free.

## Registry and stored records

- The registry's `register` method returns the id the fingerprint was filed under; the
  caller must write that id into the record, because an overflowed request filed
  under `custom-other` must not keep its original id
  (`crates/zroutery-core/src/client.rs::ClientRegistry::register`).
- `name_for_user_agent` reads a stored agent string through the same ordered
  rules, returning only id and name; it never rewrites a stored record
  (`crates/zroutery-core/src/client.rs::name_for_user_agent`).
- `registered_rules` is the diagnostics source for which clients are recognised,
  with provenance (`crates/zroutery-core/src/client.rs::registered_rules`).

## Boundaries

- No header attribution makes an unauthenticated request succeed; authentication
  runs before attribution.
- No second ladder inside a protocol.
- No guarantee that a caller-controlled `User-Agent` is truthful; confidence is
  the honest part of the answer.
- No unbounded per-client table; overflow is a visible bucket.

## Sources

- `crates/zroutery-core/src/client.rs::identify`
- `crates/zroutery-core/src/client.rs::ClientFingerprint`
- `crates/zroutery-core/src/client.rs::ClientFamily`
- `crates/zroutery-core/src/client.rs::AttributionSource`
- `crates/zroutery-core/src/client.rs::Confidence`
- `crates/zroutery-core/src/client.rs::Confidence::is_recognised`
- `crates/zroutery-core/src/client.rs::ClientId`
- `crates/zroutery-core/src/client.rs::ClientId::sanitised`
- `crates/zroutery-core/src/client.rs::MAX_CUSTOM_CLIENT_ID_LEN`
- `crates/zroutery-core/src/client.rs::MAX_TRACKED_CLIENTS`
- `crates/zroutery-core/src/client.rs::ClientRegistry`
- `crates/zroutery-core/src/client.rs::ClientRegistry::register`
- `crates/zroutery-core/src/client.rs::name_for_user_agent`
- `crates/zroutery-core/src/client.rs::registered_rules`
