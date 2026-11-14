# ADR-0009: User Documentation Scope

Status: accepted
Date: 2026-11-14
Supersedes: none

## Context

The README covers setup, routing, data locations and platform support. It does
not describe Accounts, check-in, NewAPI panels, Cutover or Takeover; those
behaviours live in the reference pages. The six README screenshots date from
2026-10-07, and the repository has no root `LICENSE` file even though
`Cargo.toml` declares MIT.

A draft added the missing sections in English, Simplified Chinese and
Traditional Chinese, plus a `LICENSE`. It was reverted in `fed5d09`.

## Decision

Do not expand the README with those sections in this round. Keep it as the
product overview; the reference pages own subsystem behaviour. Do not replace
the screenshots until they can be regenerated from the real application with
current data. Do not add a root `LICENSE` as part of this rollback.

## Why

- The only available browser for the UI harness is Helium; its lazy pages stay
  on the loading fallback, so a fresh screenshot run produced empty pages.
  Shipping those images would be worse than the dated ones.
- Three full README translations are a real maintenance cost for two
  developers; the missing workflows already have source-verified reference
  pages, and the UI itself is the primary path.
- A `LICENSE` file is a packaging/legal decision, not a documentation cleanup;
  it deserves its own change and review.

## Consequences

- The README remains a product overview; Accounts, Cutover, Takeover,
  check-in and NewAPI are documented in `docs/development/reference/` and in
  the application.
- The screenshots remain dated. `docs/images/` is not a source of truth; the
  reference pages are.
- MIT remains declared in `Cargo.toml`; a root license file is deferred.

## Revisit

Reopen this decision when a Chromium-based browser can render the built UI
reliably, or when an owner accepts the three-language README maintenance. A
root `LICENSE` can be added independently at any time.

## Sources

- `README.md`
- `README.zh-CN.md`
- `README.zh-TW.md`
- `docs/images/`
- `scripts/ui_overflow_test.py`
- `docs/development/state.json`
