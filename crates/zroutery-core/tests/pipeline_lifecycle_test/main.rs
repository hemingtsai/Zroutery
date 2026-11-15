//! Production lifecycle gates: one terminal transition per request.
//!
//! These tests drive a real Zroutery server in front of a mock provider and
//! read what the request lifecycle actually recorded: the activity record, the
//! terminal `Outcome`, the spend ledger and the router's health view. The claims
//! under test are the ones a request makes about itself — planned vs
//! last-attempted vs served identity, one classified failure path, exactly one
//! terminal transition, and never a success for an answer the client abandoned.

mod attempt_accounting;
mod continuation;
mod identity_and_classification;
mod rectifier_attempts;
mod response_storage;
mod streaming;
mod support;
mod terminal_transitions;
