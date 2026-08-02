mod app;
pub mod camera;
mod electrostatics_gpu;
pub mod gpu;
mod renderer;
pub mod scene;
mod ui;

pub use app::{RunError, run, run_for};
pub use gpu::{GpuConfig, SmokeTestError, SmokeTestReport, smoke_test};
