//! Headless domain core for Field CAD.
//!
//! This crate owns the world model, dimensional schemas, the deterministic
//! simulation clock, and the immutable snapshot types. It depends on no UI,
//! window, or graphics crate, and it must remain testable without a GPU.

pub mod domain;
pub mod ids;
pub mod sampling;
pub mod schema;
pub mod snapshot;
pub mod source_geometry;
pub mod time;
pub mod units;
pub mod world;

pub use domain::*;
pub use ids::*;
pub use sampling::*;
pub use schema::*;
pub use snapshot::*;
pub use source_geometry::*;
pub use time::*;
pub use units::*;
pub use world::*;
