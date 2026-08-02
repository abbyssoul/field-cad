use serde::{Deserialize, Serialize};

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum TimeStepError {
    #[error("simulation time step must be finite and greater than zero, received {seconds}")]
    Invalid { seconds: f64 },
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TimeStepParseError {
    #[error(
        "invalid time step '{input}'; use a number optionally followed by s, ms, us/µs, ns, ps, fs, min, or h"
    )]
    InvalidSyntax { input: String },
    #[error(transparent)]
    InvalidValue(#[from] TimeStepError),
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeStep(f64);

impl TimeStep {
    pub fn from_seconds(seconds: f64) -> Result<Self, TimeStepError> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(TimeStepError::Invalid { seconds });
        }
        Ok(Self(seconds))
    }

    pub const fn seconds(self) -> f64 {
        self.0
    }
}

impl FromStr for TimeStep {
    type Err = TimeStepParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if let Ok(seconds) = input.parse::<f64>() {
            return Self::from_seconds(seconds).map_err(Into::into);
        }

        // SI symbols are case-sensitive. Check the longer suffixes before `s`
        // so a value such as `1ms` is parsed as milliseconds.
        for (suffix, seconds_per_unit) in [
            ("min", 60.0),
            ("ms", 1.0e-3),
            ("us", 1.0e-6),
            ("µs", 1.0e-6),
            ("μs", 1.0e-6),
            ("ns", 1.0e-9),
            ("ps", 1.0e-12),
            ("fs", 1.0e-15),
            ("h", 3_600.0),
            ("s", 1.0),
        ] {
            let Some(number) = input.strip_suffix(suffix) else {
                continue;
            };
            let Ok(value) = number.trim().parse::<f64>() else {
                continue;
            };
            return Self::from_seconds(value * seconds_per_unit).map_err(Into::into);
        }

        Err(TimeStepParseError::InvalidSyntax {
            input: input.to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimulationMode {
    #[default]
    Paused,
    Running,
}

impl SimulationMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paused => "Paused",
            Self::Running => "Running",
        }
    }
}

/// What a solver is told about one accepted tick.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepContext {
    pub tick: u64,
    pub time_seconds: f64,
    pub time_step: TimeStep,
}

/// The clock's observable state: one tick's worth of context plus the transport
/// mode. `mode` is the only thing a solver is not told, because a solver only
/// ever sees accepted ticks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClockSnapshot {
    pub mode: SimulationMode,
    pub step: StepContext,
}

impl ClockSnapshot {
    pub const fn tick(self) -> u64 {
        self.step.tick
    }

    pub const fn time_seconds(self) -> f64 {
        self.step.time_seconds
    }

    pub const fn time_step(self) -> TimeStep {
        self.step.time_step
    }
}

/// Fixed-step simulation clock.
///
/// Simulation time is `epoch_seconds + ticks_since_epoch * dt` rather than an
/// accumulated sum, so a run is bit-identical regardless of how many ticks are
/// taken per call. Changing `dt` opens a new epoch at the current time instead of
/// retroactively rewriting the timestamps of ticks that already happened — probe
/// history recorded before the change stays correct.
#[derive(Clone, Debug)]
pub struct SimulationClock {
    mode: SimulationMode,
    tick: u64,
    epoch_tick: u64,
    epoch_seconds: f64,
    time_step: TimeStep,
}

impl SimulationClock {
    pub fn new(time_step: TimeStep) -> Self {
        Self {
            mode: SimulationMode::Paused,
            tick: 0,
            epoch_tick: 0,
            epoch_seconds: 0.0,
            time_step,
        }
    }

    pub fn snapshot(&self) -> ClockSnapshot {
        ClockSnapshot {
            mode: self.mode,
            step: self.context(),
        }
    }

    pub const fn mode(&self) -> SimulationMode {
        self.mode
    }

    pub fn time_seconds(&self) -> f64 {
        self.epoch_seconds + (self.tick - self.epoch_tick) as f64 * self.time_step.seconds()
    }

    pub const fn time_step(&self) -> TimeStep {
        self.time_step
    }

    /// Adopt a new numerical time step from the next tick onwards. Elapsed time
    /// is preserved; only the future spacing changes.
    pub fn set_time_step(&mut self, time_step: TimeStep) {
        if time_step == self.time_step {
            return;
        }
        self.epoch_seconds = self.time_seconds();
        self.epoch_tick = self.tick;
        self.time_step = time_step;
    }

    pub fn play(&mut self) {
        self.mode = SimulationMode::Running;
    }

    pub fn pause(&mut self) {
        self.mode = SimulationMode::Paused;
    }

    pub fn advance_running(&mut self) -> Option<StepContext> {
        (self.mode == SimulationMode::Running).then(|| self.advance())
    }

    pub fn step_once(&mut self) -> Option<StepContext> {
        (self.mode == SimulationMode::Paused).then(|| self.advance())
    }

    fn context(&self) -> StepContext {
        StepContext {
            tick: self.tick,
            time_seconds: self.time_seconds(),
            time_step: self.time_step,
        }
    }

    fn advance(&mut self) -> StepContext {
        self.tick += 1;
        self.context()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(seconds: f64) -> TimeStep {
        TimeStep::from_seconds(seconds).unwrap()
    }

    #[test]
    fn time_steps_parse_plain_scientific_and_unit_suffixed_values() {
        for (input, expected_seconds) in [
            ("432", 432.0),
            ("1.23ns", 1.23e-9),
            ("4.43e-3", 4.43e-3),
            ("7.3213e-4ms", 7.3213e-7),
            ("2.5 µs", 2.5e-6),
        ] {
            let parsed: TimeStep = input.parse().unwrap();
            let relative_error =
                (parsed.seconds() - expected_seconds).abs() / expected_seconds.abs();
            assert!(relative_error < 1.0e-14, "failed to parse {input}");
        }
    }

    #[test]
    fn time_step_text_rejects_unknown_units_and_non_positive_values() {
        assert!(matches!(
            "10kg".parse::<TimeStep>(),
            Err(TimeStepParseError::InvalidSyntax { .. })
        ));
        assert!(matches!(
            "0ns".parse::<TimeStep>(),
            Err(TimeStepParseError::InvalidValue(
                TimeStepError::Invalid { .. }
            ))
        ));
        assert!("-1ms".parse::<TimeStep>().is_err());
        assert!("NaN s".parse::<TimeStep>().is_err());
    }

    #[test]
    fn clock_only_advances_in_the_requested_mode() {
        let mut clock = SimulationClock::new(step(0.25));

        assert!(clock.advance_running().is_none());
        assert_eq!(clock.step_once().unwrap().time_seconds, 0.25);
        clock.play();
        assert!(clock.step_once().is_none());
        assert_eq!(clock.advance_running().unwrap().tick, 2);
        clock.pause();
        assert_eq!(clock.snapshot().time_seconds(), 0.5);
    }

    #[test]
    fn invalid_time_steps_are_rejected() {
        assert!(TimeStep::from_seconds(0.0).is_err());
        assert!(TimeStep::from_seconds(f64::NAN).is_err());
    }

    #[test]
    fn changing_the_time_step_does_not_rewrite_elapsed_time() {
        let mut clock = SimulationClock::new(step(0.5));
        clock.step_once();
        clock.step_once();
        assert_eq!(clock.time_seconds(), 1.0);

        clock.set_time_step(step(0.1));

        // The two ticks already taken still account for one second.
        assert_eq!(clock.time_seconds(), 1.0);
        assert_eq!(clock.step_once().unwrap().time_seconds, 1.1);
        assert_eq!(clock.snapshot().tick(), 3);
    }

    #[test]
    fn time_is_reconstructed_from_the_tick_count_not_accumulated() {
        let mut fine = SimulationClock::new(step(0.1));
        for _ in 0..10 {
            fine.step_once();
        }

        // An accumulating clock would drift away from 1.0 after ten additions
        // of 0.1; anchoring to the tick count keeps runs reproducible.
        assert_eq!(fine.time_seconds(), 1.0);
    }
}
