//! Headless domain core for Field CAD.
//!
//! This crate owns the world model, dimensional schemas, the deterministic
//! simulation clock, and the immutable snapshot types. It depends on no UI,
//! window, or graphics crate, and it must remain testable without a GPU.

pub mod domain;
pub mod hermite;
pub mod ids;
pub mod quantities;
pub mod sampling;
pub mod scene_scale;
pub mod schema;
pub mod snapshot;
pub mod source_geometry;
pub mod time;
pub mod units;
pub mod world;

pub use domain::*;
pub use hermite::*;
pub use ids::*;
pub use sampling::*;
pub use scene_scale::*;
pub use schema::*;
pub use snapshot::*;
pub use source_geometry::*;
pub use time::*;
pub use units::*;
pub use world::*;
