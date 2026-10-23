//! The view half of `.dash` dashboards, ported from DuckLocal: a spec as a
//! workspace tab. Parsing, validation, filters and comments are the
//! `khonsu-dash` crate's; this module draws.

pub use khonsu_dash::{comments, filter, model};

pub mod comment_card;
pub mod complete;
pub mod highlight;
pub mod plot;
pub mod prepare;
pub mod presence;
pub mod tabs;
pub mod view;
pub mod watch;
mod wheel;
