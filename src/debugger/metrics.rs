use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize)]
pub enum Action {
    GenerateDebugUnits,
    PrepareDebugUnits,
    GenerateSteps,
    GenerateVariableDefinitions,
}

pub struct MetricsRecorder {
    last_capture: Instant,
    pub metrics: Vec<(Action, Duration)>,
}

impl MetricsRecorder {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            last_capture: now,
            metrics: Vec::new(),
        }
    }

    pub fn capture(&mut self, action: Action) {
        let now = Instant::now();
        let duration = now.duration_since(self.last_capture);
        self.metrics.push((action, duration));
        self.last_capture = now;
    }
}

impl Default for MetricsRecorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_basic_recording() {
        let mut recorder = MetricsRecorder::new();

        thread::sleep(Duration::from_millis(10));
        recorder.capture(Action::GenerateDebugUnits);

        thread::sleep(Duration::from_millis(20));
        recorder.capture(Action::PrepareDebugUnits);

        let metrics = recorder.metrics;
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].0, Action::GenerateDebugUnits);
        assert_eq!(metrics[1].0, Action::PrepareDebugUnits);

        // Check that durations are reasonable (allowing for some variance)
        assert!(metrics[0].1 >= Duration::from_millis(8));
        assert!(metrics[1].1 >= Duration::from_millis(18));
    }
}
