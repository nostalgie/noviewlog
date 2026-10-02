//! Shared UI state helpers (click tracking).

use std::time::{Duration, Instant};

/// Multi-click window (double/triple select).
const CLICK_INTERVAL: Duration = Duration::from_millis(400);
const CLICK_SLOP_PX: f32 = 6.0;

pub(crate) struct ClickTracker {
    last_at: Option<Instant>,
    last_x: f32,
    last_y: f32,
    count: u32,
}

impl ClickTracker {
    pub(crate) fn new() -> Self {
        Self {
            last_at: None,
            last_x: 0.0,
            last_y: 0.0,
            count: 0,
        }
    }

    pub(crate) fn on_press(&mut self, x: f32, y: f32) -> u32 {
        let now = Instant::now();
        let same = self
            .last_at
            .map(|t| now.duration_since(t) <= CLICK_INTERVAL)
            .unwrap_or(false)
            && (x - self.last_x).abs() <= CLICK_SLOP_PX
            && (y - self.last_y).abs() <= CLICK_SLOP_PX;
        self.count = if same { (self.count + 1).min(3) } else { 1 };
        self.last_at = Some(now);
        self.last_x = x;
        self.last_y = y;
        self.count
    }
}
