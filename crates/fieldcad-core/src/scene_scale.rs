//! How many metres one render/camera unit represents.
//!
//! Every stored position in this crate is SI metres, unscaled — see
//! [ADR 0004](../../../docs/adr/0004-si-units-in-the-core.md). `SceneScale` is
//! not a second way to store a position; it is the conversion factor a
//! renderer applies at the boundary where an SI `f64` world position becomes
//! an `f32` camera/render-space value, so a nanometre-scale or
//! astronomical-scale scene renders at a magnitude the camera's fixed
//! near/far and dolly-distance clamps were tuned for. Nothing a solver reads
//! is affected by it.

use serde::{Deserialize, Serialize};

use crate::quantities::{LengthMetres, meter};

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum SceneScaleError {
    #[error("scene scale must be finite and greater than zero, received {metres} m")]
    Invalid { metres: f64 },
}

/// Metres represented by one render/camera unit.
///
/// Internally wraps a [`LengthMetres`] so the type system records this as a
/// length rather than a bare `f64`. Defaults to [`SceneScale::metre`] (1.0),
/// which reproduces the desktop app's original behaviour exactly: a scene
/// that never touches this setting renders precisely as it did before this
/// type existed.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SceneScale(LengthMetres);

impl Default for SceneScale {
    fn default() -> Self {
        Self::metre()
    }
}

impl SceneScale {
    pub fn from_metres(metres: f64) -> Result<Self, SceneScaleError> {
        if !metres.is_finite() || metres <= 0.0 {
            return Err(SceneScaleError::Invalid { metres });
        }
        Ok(Self(LengthMetres::new::<meter>(metres)))
    }

    pub fn metres(self) -> f64 {
        self.0.get::<meter>()
    }

    /// The typed length quantity this scale wraps.
    pub const fn quantity(self) -> LengthMetres {
        self.0
    }

    pub fn nanometre() -> Self {
        Self::from_metres(1.0e-9).expect("1e-9 is a valid scene scale")
    }

    pub fn micrometre() -> Self {
        Self::from_metres(1.0e-6).expect("1e-6 is a valid scene scale")
    }

    pub fn millimetre() -> Self {
        Self::from_metres(1.0e-3).expect("1e-3 is a valid scene scale")
    }

    /// The default: one render unit is one metre.
    pub fn metre() -> Self {
        Self::from_metres(1.0).expect("1.0 is a valid scene scale")
    }

    pub fn kilometre() -> Self {
        Self::from_metres(1.0e3).expect("1e3 is a valid scene scale")
    }

    pub fn astronomical_unit() -> Self {
        Self::from_metres(1.495_978_707e11).expect("1 AU is a valid scene scale")
    }

    pub fn light_year() -> Self {
        Self::from_metres(9.460_730_472_580_8e15).expect("1 light-year is a valid scene scale")
    }

    /// Convert an SI-metre world value into a render-space value, dividing
    /// before the cast to `f32` so the result's magnitude — not its absolute
    /// distance from an arbitrary origin — determines how much of `f32`'s
    /// precision it uses.
    pub fn to_render(self, world_metres: f64) -> f32 {
        (world_metres / self.metres()) as f32
    }

    /// The inverse of [`Self::to_render`]: a render-space value back to SI
    /// metres.
    pub fn to_world(self, render_units: f32) -> f64 {
        f64::from(render_units) * self.metres()
    }

    pub fn to_render_vec3(self, world: glam::DVec3) -> glam::Vec3 {
        glam::Vec3::new(
            self.to_render(world.x),
            self.to_render(world.y),
            self.to_render(world.z),
        )
    }

    pub fn to_world_vec3(self, render: glam::Vec3) -> glam::DVec3 {
        glam::DVec3::new(
            self.to_world(render.x),
            self.to_world(render.y),
            self.to_world(render.z),
        )
    }

    pub fn to_render_vec2(self, world: glam::DVec2) -> glam::Vec2 {
        glam::Vec2::new(self.to_render(world.x), self.to_render(world.y))
    }

    pub fn to_world_vec2(self, render: glam::Vec2) -> glam::DVec2 {
        glam::DVec2::new(self.to_world(render.x), self.to_world(render.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_scales_are_rejected() {
        assert!(SceneScale::from_metres(0.0).is_err());
        assert!(SceneScale::from_metres(-1.0).is_err());
        assert!(SceneScale::from_metres(f64::NAN).is_err());
        assert!(SceneScale::from_metres(f64::INFINITY).is_err());
    }

    #[test]
    fn default_scale_is_one_metre_per_unit() {
        assert_eq!(SceneScale::default(), SceneScale::metre());
        assert_eq!(SceneScale::default().metres(), 1.0);
    }

    #[test]
    fn metre_scale_render_conversion_is_the_identity() {
        let scale = SceneScale::metre();
        assert_eq!(scale.to_render(12.5), 12.5_f32);
        assert_eq!(scale.to_world(12.5_f32), 12.5_f64);
    }

    #[test]
    fn nanometre_scale_brings_tiny_positions_to_unit_magnitude() {
        let scale = SceneScale::nanometre();
        // A 2 nm radius, expressed in metres, renders as render-space 2.0 —
        // comfortably inside the camera's fixed distance/near/far window,
        // instead of collapsing to ~2e-9 under the old unscaled cast.
        let render = scale.to_render(2.0e-9);
        assert!((render - 2.0).abs() < 1.0e-6, "got {render}");
    }

    #[test]
    fn astronomical_scale_brings_huge_positions_to_unit_magnitude() {
        let scale = SceneScale::astronomical_unit();
        let render = scale.to_render(scale.metres() * 3.0);
        assert!((render - 3.0).abs() < 1.0e-4, "got {render}");
    }

    #[test]
    fn vec3_round_trip_matches_scalar_conversion() {
        let scale = SceneScale::micrometre();
        let world = glam::DVec3::new(1.0e-6, 2.0e-6, -3.0e-6);
        let render = scale.to_render_vec3(world);
        assert!((render.x - 1.0).abs() < 1.0e-4);
        assert!((render.y - 2.0).abs() < 1.0e-4);
        assert!((render.z + 3.0).abs() < 1.0e-4);

        let back = scale.to_world_vec3(render);
        assert!((back - world).length() < 1.0e-9);
    }
}
