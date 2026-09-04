//! Media handling: finding the images a request carries, describing them
//! with a vision model, and swapping the descriptions in.
//!
//! Three small pieces instead of one framework — collection, description and
//! replacement each fit in a page, and the pipeline is the only thing that
//! knows how they connect:
//!
//! ```text
//! request ──collect──▶ images ──describe──▶ texts ──transform──▶ request
//!                                        └─fail──▶ placeholder
//! ```
//!
//! The proactive path (target model cannot see, describe before sending) and
//! the reactive path (upstream rejected the image, describe and retry) use
//! the same three pieces — only the trigger differs.

pub mod collect;
pub mod transform;
pub mod vision;
