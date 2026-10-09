//! Askama page and fragment view models, one module per slice. Re-exported
//! from the crate root, so handlers keep naming them `crate::HomePage` etc.

mod account;
mod community;
mod discovery;
mod layout;
mod moderation;

pub use account::*;
pub use community::*;
pub use discovery::*;
pub use layout::*;
pub use moderation::*;
