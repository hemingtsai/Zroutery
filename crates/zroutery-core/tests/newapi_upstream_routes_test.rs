//! The adapter's endpoint table, checked against upstream's own route table.
//!
//! # Why this exists
//!
//! Every other test in the adapter runs against an in-process fake panel whose
//! handlers were written by the same author as the parser. That is a genuine
//! local mock-server contract -- a real `axum` server on an ephemeral port --
//! but it cannot answer the one question that matters most: **does the adapter
//! call endpoints that exist?** A fake serves whatever the parser expects, so a
//! misreading of the protocol is invisible: both sides agree on the same wrong
//! path.
//!
//! # Provenance of the table below
//!
//! `UPSTREAM_ROUTES` is transcribed from `router/api-router.go` at
//! `Calcium-Ion/new-api` commit
//! `c2b7a9a9e0b548c2051a949fceabb59029adcb49` (2026-09-25) -- the commit the
//! adapter's own module documentation cites. Each entry is the method, the path,
//! and the middleware guarding it, as written there.
//!
//! # Why the calls are read out of the source
//!
//! An earlier draft of this file hand-listed the adapter's endpoints next to the
//! upstream table and compared the two lists. That has the same defect it was
//! written to catch: two lists, one author, so adding a call and forgetting the
//! local list would have passed. So the call side is *extracted from the adapter
//! source* rather than written down here, and only the upstream side is a human
//! transcription. A call with no upstream row now fails, and it fails because the
//! code asked for something the table does not have.
//!
//! # What this does and does not establish
//!
//! It establishes that every request the adapter can make is a request upstream
//! registers, with the same method, and that the table describes the adapter
//! rather than a wish list -- both directions, neither hand-maintained on the call
//! side.
//!
//! It does **not** re-verify the transcription against upstream automatically.
//! Upstream is not fetched at test time, deliberately: a test that needs the
//! network fails for reasons unrelated to the code. So the upstream side is still
//! a human reading a route table, and this is a fence around that human, not a
//! replacement for them. Re-verify by re-reading the route table at a newer
//! commit -- and see below, because the OpenAPI spec alone would mislead.
//!
//! # Why the spec is not the source
//!
//! `docs/openapi/api.json` at the same commit documents 136 paths and contains
//! **neither `/api/subscription/self` nor `/api/user/checkin`** -- the two
//! endpoints behind the subscription/quota rules and the whole check-in path.
//! Both exist in the Go router. The spec is incomplete, so "the spec does not list
//! it" is not evidence that an endpoint is absent, and treating it as such would
//! have deleted two working endpoints.

// Gated on `newapi` (which implies `account`), following the twenty existing
// test targets that read a feature-gated module. Without this the target fails to
// *compile* under `cargo test --workspace`, which is a default-features run -- so
// this gate is load-bearing for CI, not tidiness.
#![cfg(feature = "newapi")]

use zroutery_core::account::adapters::newapi::UPSTREAM_ROUTES;

/// The adapter's own source, so the call side of this comparison is read rather
/// than written. Compiled in, so editing the adapter recompiles this too.
const ADAPTER_SOURCE: &str = include_str!("../src/account/adapters/newapi.rs");

const METHOD_MARKER: &str = "reqwest::Method::";

/// The point where the adapter's test module begins. Everything after it is mock
/// panel wiring, which serves whatever routes the tests declare and so is not
/// evidence of what production calls.
const TEST_MODULE_MARKER: &str = "#[cfg(test)]";

/// Every `(method, path)` the adapter requests in production code.
///
/// Reads each `reqwest::Method::<VERB>` and takes the next string literal after
/// it, which is the path argument of the same call. That pairing is safe to rely
/// on because the helper methods take the method first and the path second, and
/// the path is always a literal rather than a built string -- every paged or
/// windowed endpoint passes its query separately.
///
/// The `UPSTREAM_ROUTES` entries cannot be picked up by this, which is what keeps
/// the extraction honest: they are written `("GET", "/api/...")` with no method
/// marker, so only real call sites match. The module doc's endpoint table is
/// likewise skipped, being backtick-quoted rather than a quoted literal.
fn calls_the_adapter_makes() -> Vec<(&'static str, &'static str)> {
    let production = ADAPTER_SOURCE
        .split_once(TEST_MODULE_MARKER)
        .map_or(ADAPTER_SOURCE, |(head, _)| head);

    let mut calls = Vec::new();
    let mut rest = production;

    while let Some(at) = rest.find(METHOD_MARKER) {
        let after = &rest[at + METHOD_MARKER.len()..];
        rest = after;

        let method = if after.starts_with("GET") {
            "GET"
        } else if after.starts_with("POST") {
            "POST"
        } else {
            continue;
        };
        let tail = &after[method.len()..];

        let Some(open) = tail.find('"') else { continue };
        let literal = &tail[open + 1..];
        let Some(close) = literal.find('"') else {
            continue;
        };
        let path = &literal[..close];
        if path.starts_with("/api/") {
            calls.push((method, path));
        }
    }

    calls
}

/// The distinct pairs, order preserved, so a repeated call site is not counted
/// twice and a comparison is not confused by duplicates.
fn distinct<'a>(calls: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut out: Vec<(&'a str, &'a str)> = Vec::new();
    for call in calls {
        if !out.contains(call) {
            out.push(*call);
        }
    }
    out
}

#[test]
fn every_request_the_adapter_makes_is_registered_upstream() {
    let calls = calls_the_adapter_makes();
    assert!(
        !calls.is_empty(),
        "extraction found no requests at all, so every assertion below would pass \\
         vacuously. If the adapter's request helpers were renamed or restructured, \\
         this file needs updating rather than reporting success."
    );

    let mut undeclared: Vec<String> = Vec::new();
    for (method, path) in distinct(&calls) {
        // Matched on method *and* path, not path alone. `/api/user/checkin` is
        // registered twice upstream -- GET for status, POST for the claim -- so a
        // path-only lookup finds the GET row and reports a method mismatch on a
        // pair that exists. That was this file's own first draft failing.
        let declared = UPSTREAM_ROUTES
            .iter()
            .any(|(m, p, _)| *m == method && *p == path);

        if !declared {
            let path_exists = UPSTREAM_ROUTES.iter().any(|(_, p, _)| *p == path);
            undeclared.push(if path_exists {
                format!("{method} {path} - the path exists upstream but not with this method")
            } else {
                format!("{method} {path} - no such path in the route table")
            });
        }
    }

    assert!(
        undeclared.is_empty(),
        "the adapter requests endpoints the upstream route table does not register: \\
         {undeclared:?}"
    );
}

#[test]
fn the_route_table_describes_the_adapter_rather_than_a_wish_list() {
    // The other direction. If the adapter stops calling an endpoint but the table
    // keeps it, the table stops describing the adapter and starts describing an
    // intention -- and the next person to read it is misled about what is called,
    // which is the same failure this file exists to prevent, pointing the other
    // way.
    let called = distinct(&calls_the_adapter_makes());

    let tabled: Vec<(&str, &str)> = UPSTREAM_ROUTES
        .iter()
        .map(|(method, path, _)| (*method, *path))
        .collect();

    let surplus: Vec<(&str, &str)> = tabled
        .into_iter()
        .filter(|entry| !called.contains(entry))
        .collect();

    assert!(
        surplus.is_empty(),
        "the route table lists routes the adapter does not request: {surplus:?}. Either the \\
         adapter stopped calling one, or the table needs trimming -- a table that \\
         overstates what is called is worse than no table."
    );
}

#[test]
fn the_route_table_records_how_upstream_guards_each_endpoint() {
    // The method is only half of a call. `middleware.UserAuth()` versus nothing
    // decides whether a request is answered or refused, and a transcription that
    // dropped the guard would make an unauthenticated probe look correct.
    for (method, path, guard) in UPSTREAM_ROUTES {
        assert!(
            !guard.is_empty(),
            "{method} {path} has no recorded guard; every route in this table is behind \
             at least one middleware and an empty guard means the transcription dropped it"
        );
    }

    let user_auth: Vec<&str> = UPSTREAM_ROUTES
        .iter()
        .filter(|(_, _, guard)| guard.contains("UserAuth"))
        .map(|(_, path, _)| *path)
        .collect();

    for expected in [
        "/api/user/self",
        "/api/log/self",
        "/api/log/self/stat",
        "/api/subscription/self",
        "/api/user/checkin",
    ] {
        assert!(
            user_auth.contains(&expected),
            "{expected} is served behind UserAuth upstream; if the adapter treats it as \
             public it will read a 401 as data. Guarded: {user_auth:?}"
        );
    }

    // And the one that must stay public, because the adapter reads it before it
    // has any credential at all.
    let status = UPSTREAM_ROUTES
        .iter()
        .find(|(_, path, _)| *path == "/api/status")
        .expect("the status route is transcribed");
    assert_eq!(
        status.2, "none",
        "/api/status is anonymous upstream, which is what lets probe_status work before \
         authenticating. If it ever gains a guard, that is a behaviour change here."
    );
}

#[test]
fn no_route_is_registered_twice() {
    // A path registered twice must differ by method, since `POST /x` and
    // `GET /x` are different endpoints. A true duplicate is a transcription slip
    // that would also make the method lookup ambiguous.
    for (index, (method, path, _)) in UPSTREAM_ROUTES.iter().enumerate() {
        for (other_method, other_path, _) in UPSTREAM_ROUTES.iter().skip(index + 1) {
            assert!(
                method != other_method || path != other_path,
                "({method} {path}) appears more than once in the route table"
            );
        }
    }
}

#[test]
fn the_two_endpoints_the_openapi_spec_omits_are_still_transcribed() {
    // Recorded as a finding rather than as a comment, because the instinct to
    // "fix" this table by deleting the two entries is exactly the wrong move: they
    // exist in the Go router and the spec is what is incomplete.
    for path in ["/api/subscription/self", "/api/user/checkin"] {
        assert!(
            UPSTREAM_ROUTES
                .iter()
                .any(|(_, candidate, _)| *candidate == path),
            "{path} was removed from the route table. It is absent from the upstream \
             OpenAPI spec and present in the upstream Go router. Confirm against \
             router/api-router.go before deleting anything here."
        );
    }
}
