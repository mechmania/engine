use cpu_time::ProcessTime;
use simple_moving_average::{SumTreeSMA, SMA};
use std::time::Duration;

/// Wall-clock ceiling per tick
pub const TICK_HANG_TIMEOUT: Duration = Duration::from_secs(5);

const TOTAL_COMPUTE_TICKS: u32 = 50000;
const DELAY_TICKS: u32 = 1000;

/// Tracks a single bot's banked CPU-time compute budget, in units of "ticks" (one tick's
/// worth of the engine's own recent per-tick CPU time, see [`EngineTickClock`]).
pub struct ComputeBudget {
    ticks: u32,
}

impl ComputeBudget {
    pub fn new() -> Self {
        Self { ticks: TOTAL_COMPUTE_TICKS }
    }

    /// Charge (or refund) budget for a tick that consumed `cpu_time` of the bot's own CPU,
    /// relative to `engine_time` (the engine's own recent average per-tick CPU cost). Returns
    /// the number of ticks charged, for logging.
    pub fn charge(&mut self, cpu_time: Duration, engine_time: Duration) -> u32 {
        let elapsed = cpu_time.div_duration_f64(engine_time) as u32;
        self.ticks = if elapsed <= DELAY_TICKS {
            TOTAL_COMPUTE_TICKS.min(self.ticks + DELAY_TICKS - elapsed)
        } else {
            self.ticks - self.ticks.min(elapsed - DELAY_TICKS)
        };
        elapsed
    }

    /// Forfeit all remaining budget — used when a bot blows through [`TICK_HANG_TIMEOUT`]
    /// and we have no valid CPU-time reading to charge it accurately.
    pub fn forfeit(&mut self) {
        self.ticks = 0;
    }
}

/// Tracks the engine's own recent per-tick CPU cost (a moving average), used as the
/// reference unit that bot CPU time is measured against in [`ComputeBudget::charge`].
pub struct EngineTickClock {
    ma: SumTreeSMA<Duration, u32, 50>,
}

impl EngineTickClock {
    pub fn new() -> Self {
        Self { ma: SumTreeSMA::from_zero(Duration::from_millis(1)) }
    }

    pub fn average(&self) -> Duration {
        self.ma.get_average()
    }

    /// Time a closure (expected to be the engine's own `eval_tick` call) using the engine's
    /// own process CPU time, and fold the sample into the moving average.
    pub fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = ProcessTime::now();
        let ret = f();
        self.ma.add_sample(start.elapsed());
        ret
    }
}
