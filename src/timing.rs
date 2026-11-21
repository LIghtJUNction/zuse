//! Monotonic timing for persistent CLI, MCP and embedded runtimes.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Timer {
    started: Option<Instant>,
    stopped: Duration,
    command_total: Duration,
    commands: u64,
}

impl Timer {
    pub fn running(&self) -> bool {
        self.started.is_some()
    }
    pub fn start(&mut self) {
        if !self.running() {
            self.reset();
        }
    }
    pub fn reset(&mut self) {
        self.started = Some(Instant::now());
        self.stopped = Duration::ZERO;
        self.command_total = Duration::ZERO;
        self.commands = 0;
    }
    pub fn stop(&mut self) {
        if let Some(started) = self.started.take() {
            self.stopped = started.elapsed();
        }
    }
    pub fn record(&mut self, elapsed: Duration, counted: bool) -> String {
        if counted {
            self.command_total = self.command_total.saturating_add(elapsed);
            self.commands = self.commands.saturating_add(1);
        }
        let total = self
            .started
            .map_or(self.stopped, |started| started.elapsed());
        format!(
            "{{\"running\":{},\"command_us\":{},\"total_elapsed_us\":{},\"command_total_us\":{},\"commands\":{}}}",
            self.running(),
            elapsed.as_micros(),
            total.as_micros(),
            self.command_total.as_micros(),
            self.commands
        )
    }
    pub fn status(&mut self) -> String {
        self.record(Duration::ZERO, false)
    }
}
