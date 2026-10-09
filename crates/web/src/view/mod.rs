//! View models: translate application/domain types into presentation-ready
//! data for Askama templates (labels, formatting, CSS classes).
//!
//! Business rules stay in the application/domain layers; this module only
//! formats, maps and localizes. Every user-facing label goes through the
//! request's [`Translator`].
//!
//! Split by slice: [`format`] holds the shared label/number/date helpers the
//! other modules build on; each remaining module serves one area of the site.
//! Everything is re-exported here, so callers keep using `view::…`.

mod admin;
mod community;
mod format;
mod moderation;
mod privacy;
mod search;

pub use admin::*;
pub use community::*;
pub use format::*;
pub use moderation::*;
pub use privacy::*;
pub use search::*;

#[cfg(test)]
mod tests;
