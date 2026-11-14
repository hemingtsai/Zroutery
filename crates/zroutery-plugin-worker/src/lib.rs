//! Pure VM implementation. Only the sandboxed binary may execute package bytes
//! in production; unit tests use synthetic inputs, never user data.
mod loader;
pub mod runtime;
