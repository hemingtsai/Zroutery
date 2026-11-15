#![cfg(feature = "ml")]
//! End to end tests: a real Zroutery server in front of a mock provider.
//!
//! These cover the paths that unit tests cannot: cross dialect streaming,
//! failover between providers, auth and the model listing.

mod accounting;
mod accounts;
mod budget;
mod dialect;
mod failover;
mod gateway;
mod models;
mod routing;
mod shadow;
mod shadow_ab;
mod support;
mod upstream;
