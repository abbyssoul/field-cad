mod app;
pub mod camera;
mod electromagnetism_gpu;
mod electrostatics_gpu;
pub mod gpu;
mod mcp;
mod renderer;
pub mod scene;
mod ui;

pub use app::{RunError, run, run_for};
pub use gpu::{GpuConfig, SmokeTestError, SmokeTestReport, smoke_test};
