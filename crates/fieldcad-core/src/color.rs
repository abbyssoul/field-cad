//! A cosmetic, display-only per-object color — see `WorldObject::color`.
//!
//! Not a physical quantity: no `uom` dimension applies, so this is a plain
//! `f32` RGBA value rather than a `Quantity`. `f32`, not `f64`, because its
//! only consumer is the renderer's `[f32; 4]` instance tint — carrying `f64`
//! precision through a value nothing ever computes with would be pretend
//! accuracy.

use serde::{Deserialize, Serialize};

use crate::world::WorldError;

/// An RGBA color in `0.0..=1.0` per channel, straight-multiplied against a
/// mesh's shaded vertex color (see the desktop renderer's `scene.wgsl`) —
/// not a physically-based material, just a tint for telling objects apart.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl ObjectColor {
    /// An opaque color (`a = 1.0`), the common case for an object tint.
    pub const fn opaque(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub fn validate(&self) -> Result<(), WorldError> {
        let channels = [self.r, self.g, self.b, self.a];
        if channels
            .iter()
            .all(|channel| channel.is_finite() && (0.0..=1.0).contains(channel))
        {
            Ok(())
        } else {
            Err(WorldError::InvalidColor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_in_range_validates() {
        assert!(ObjectColor::opaque(0.2, 0.56, 0.88).validate().is_ok());
    }

    #[test]
    fn a_non_finite_channel_is_rejected() {
        assert_eq!(
            ObjectColor::opaque(f64::NAN as f32, 0.0, 0.0).validate(),
            Err(WorldError::InvalidColor)
        );
    }

    #[test]
    fn an_out_of_range_channel_is_rejected() {
        assert_eq!(
            ObjectColor::opaque(1.5, 0.0, 0.0).validate(),
            Err(WorldError::InvalidColor)
        );
        assert_eq!(
            ObjectColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: -0.1,
            }
            .validate(),
            Err(WorldError::InvalidColor)
        );
    }
}
