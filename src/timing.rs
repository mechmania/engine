use cpu_time::ProcessTime;
use simple_moving_average::{SumTreeSMA, SMA};
use std::time::Duration;

/// Wall-clock ceiling per tick
pub const TICK_HANG_TIMEOUT: Duration = Duration::from_secs(5);

// Declared in `ipc.rs`, not here: that file is hardlinked into every Rust bot crate, so a
// bot can compare `get_budget().remaining` against the same numbers the engine enforces.
use crate::ipc::{COMPUTE_BANK_TICKS as TOTAL_COMPUTE_TICKS, COMPUTE_REFILL_TICKS as DELAY_TICKS};

/// Tracks a single bot's banked CPU-time compute budget, in units of "ticks" (one tick's
/// worth of the engine's own recent per-tick CPU time, see [`EngineTickClock`]).
///
/// The bank refills by `DELAY_TICKS` a tick up to `TOTAL_COMPUTE_TICKS`, and may go
/// negative: an overspend is a debt, paid off by sitting out ticks until the bank is
/// positive again. Forgiving it instead would let a bot burn just under
/// [`TICK_HANG_TIMEOUT`] on every other tick.
pub struct ComputeBudget {
    ticks: i64,
}

impl ComputeBudget {
    pub fn new() -> Self {
        Self { ticks: TOTAL_COMPUTE_TICKS }
    }

    /// Charge (or refund) budget for a tick that consumed `cpu_time` of the bot's own CPU,
    /// relative to `engine_time` (the engine's own recent average per-tick CPU cost). Returns
    /// the number of ticks charged, for logging.
    pub fn charge(&mut self, cpu_time: Duration, engine_time: Duration) -> u64 {
        // No engine tick measured yet (tick 0): nothing to charge against.
        let elapsed = if engine_time.is_zero() {
            0
        } else {
            // saturating float -> int cast
            cpu_time.div_duration_f64(engine_time) as u64
        };
        self.ticks = TOTAL_COMPUTE_TICKS.min(
            self.ticks.saturating_add(DELAY_TICKS).saturating_sub(elapsed.min(i64::MAX as u64) as i64),
        );
        elapsed
    }

    /// Ticks left in the bank, negative when the bot is in debt. Published to the bot each
    /// tick (`EngineChannel::set_budget`) so it can decide what it can afford.
    pub fn remaining(&self) -> i64 {
        self.ticks
    }

    /// Out of budget, or in debt: the bot is not called at all; see [`Self::refill`].
    pub fn is_exhausted(&self) -> bool {
        self.ticks <= 0
    }

    /// Credit a tick the bot sat out: the same refill as a tick that cost nothing.
    pub fn refill(&mut self) {
        self.ticks = TOTAL_COMPUTE_TICKS.min(self.ticks + DELAY_TICKS);
    }

    /// Forfeit all remaining budget — used when a bot blows through [`TICK_HANG_TIMEOUT`]
    /// and we have no valid CPU-time reading to charge it accurately.
    pub fn forfeit(&mut self) {
        self.ticks = self.ticks.min(0);
    }
}

/// Tracks the engine's own recent per-tick CPU cost (a moving average), used as the
/// reference unit that bot CPU time is measured against in [`ComputeBudget::charge`].
pub struct EngineTickClock {
    ma: SumTreeSMA<Duration, u32, 50>,
}

impl EngineTickClock {
    pub fn new() -> Self {
        Self { ma: SumTreeSMA::from_zero(Duration::ZERO) }
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

#[cfg(test)]
mod timing_test {
    use super::*;

    #[test]
    fn overdraw_is_a_debt() {
        let tick = Duration::from_millis(1);
        let mut b = ComputeBudget::new();
        assert!(!b.is_exhausted());
        // Costing exactly the refill leaves a full bank full.
        b.charge(tick * DELAY_TICKS as u32, tick);
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS);
        // Overspend the bank by ten ticks' refill: ten sat-out ticks to pay it back.
        b.charge(tick * (TOTAL_COMPUTE_TICKS + 11 * DELAY_TICKS) as u32, tick);
        assert_eq!(b.remaining(), -10 * DELAY_TICKS);
        for _ in 0..10 {
            assert!(b.is_exhausted());
            b.refill();
        }
        assert!(b.is_exhausted());
        b.refill();
        assert!(!b.is_exhausted());
    }

    #[test]
    fn absurd_charge_does_not_overflow() {
        let mut b = ComputeBudget::new();
        b.charge(Duration::from_secs(3600), Duration::from_nanos(1));
        assert!(b.is_exhausted());
    }

    #[test]
    fn unmeasured_engine_charges_nothing() {
        let mut b = ComputeBudget::new();
        assert_eq!(b.charge(Duration::from_secs(1), Duration::ZERO), 0);
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS);
    }

    #[test]
    fn clock_starts_at_zero_and_tracks_samples() {
        let mut c = EngineTickClock::new();
        assert!(c.average().is_zero());
        for _ in 0..3 {
            c.ma.add_sample(Duration::from_micros(4));
        }
        assert_eq!(c.average(), Duration::from_micros(4));
    }

    /// `remaining` is what the bot is told each tick, so it has to track every path the
    /// bank moves on -- including `forfeit`, which zeroes a positive bank but must not
    /// forgive an existing debt.
    #[test]
    fn remaining_tracks_every_path() {
        let tick = Duration::from_millis(1);
        let mut b = ComputeBudget::new();
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS);

        // A cheap tick sits at the cap: the refill has nowhere to go.
        b.charge(tick, tick);
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS);

        // Spend below the cap and the refill becomes visible.
        b.charge(tick * 10_000, tick);
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS - 10_000 + DELAY_TICKS);
        b.refill();
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS - 10_000 + 2 * DELAY_TICKS);

        b.forfeit();
        assert_eq!(b.remaining(), 0, "a forfeit zeroes a positive bank");

        // In debt, a forfeit must leave the debt where it is rather than clearing it.
        b.charge(tick * (5 * DELAY_TICKS as u32), tick);
        assert_eq!(b.remaining(), -4 * DELAY_TICKS);
        b.forfeit();
        assert_eq!(b.remaining(), -4 * DELAY_TICKS, "a forfeit must not pay off a debt");
    }

    #[test]
    fn refill_caps_at_bank() {
        let mut b = ComputeBudget::new();
        b.refill();
        assert_eq!(b.remaining(), TOTAL_COMPUTE_TICKS);
    }
}
