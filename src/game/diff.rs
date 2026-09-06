use serde_json::{json, Map, Value};

use crate::game::config::BOTS_MAX;
use crate::game::state::{BotArray, BotState};

/// Re-exported so `use crate::game::diff::Diff;` brings in both the trait and its
/// derive, the way `serde` re-exports `Serialize`.
pub use mm_macros::Diff;

pub trait Diff {
    /// `None` when the two values are equal, otherwise a sparse JSON object holding
    /// the *new* value of each field that changed -- the old value is not recorded,
    /// since the gamelog consumer merges diffs forward onto a running state.
    ///
    /// Leaves are atomic: `pos` has no `Diff` impl, so a moved bot emits the whole
    /// `{"x":..,"y":..}` rather than a partial.
    ///
    /// ```json
    /// { "tick": 42, "fleet_a": { "changed": { "0": { "health": 7.5 } } } }
    /// ```
    fn diff_json(a: &Self, b: &Self) -> Option<serde_json::Value>;
}

// Not derivable: `mask` gates `arr`, so a positional field-by-field comparison
// would report changes on dead slots whose stale `BotState` data still differs.
impl Diff for BotArray {
    fn diff_json(a: &Self, b: &Self) -> Option<serde_json::Value> {
        let mut added = Map::new();
        let mut removed = Vec::new();
        let mut changed = Map::new();

        for i in 0..BOTS_MAX {
            match (a.mask[i], b.mask[i]) {
                (false, true) => {
                    added.insert(i.to_string(), json!(&b.arr[i]));
                }
                (true, false) => removed.push(json!(i)),
                (true, true) => {
                    if let Some(d) = BotState::diff_json(&a.arr[i], &b.arr[i]) {
                        changed.insert(i.to_string(), d);
                    }
                }
                // dead in both: whatever is left in `arr` is stale, never report it
                (false, false) => {}
            }
        }

        if added.is_empty() && removed.is_empty() && changed.is_empty() {
            return None;
        }

        let mut res = Map::new();
        if !added.is_empty() {
            res.insert("added".to_owned(), Value::Object(added));
        }
        if !removed.is_empty() {
            res.insert("removed".to_owned(), Value::Array(removed));
        }
        if !changed.is_empty() {
            res.insert("changed".to_owned(), Value::Object(changed));
        }
        Some(Value::Object(res))
    }
}

#[cfg(feature = "engine")]
#[cfg(test)]
mod diff_test {
    use super::*;
    use crate::game::config::{BotConfig, Deposit, GameConfig, MapTile, PayloadConfig, DEPOSITS_MAX, MAP_SIZE, PAYLOAD_PATH};
    use crate::game::state::{BotClass, GameState, StateOption};
    use crate::game::util::Vec2;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            bot: BotConfig {
                radius: 1.0,
                base_speed: 1.0,
                base_health: 10.0,
                base_turn_speed: 1.0,
                base_blaster_cooldown: 5,
                base_invulnerability_ticks: 3,
                base_blaster_range: 10.0,
                base_blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
            },
            payload: PayloadConfig {
                radius: 1.0,
                capture_radius: 3.0,
                speed_per_bot: 0.01,
                max_speed: 0.04,
                contest_diff: 1,
            },
            payload_path: PAYLOAD_PATH,
            deposit_count: 0,
            deposits: [Deposit::default(); DEPOSITS_MAX],
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    fn bot(id: u8) -> BotState {
        BotState {
            id,
            class: BotClass::Battle,
            health: 10.0,
            pos: Vec2::ZERO,
            vel: Vec2::ZERO,
            angle: 0.0,
            turn_vel: 0.0,
            next_fire_tick: 0,
            invulnerable_until_tick: 0,
            shot: StateOption::None,
        }
    }

    #[test]
    fn bot_state_unchanged_is_none() {
        assert!(BotState::diff_json(&bot(0), &bot(0)).is_none());
    }

    #[test]
    fn bot_state_reports_only_changed_fields() {
        let a = bot(0);
        let mut b = a.clone();
        b.health = 7.5;

        let d = BotState::diff_json(&a, &b).unwrap();
        assert_eq!(d, json!({ "health": 7.5 }));

        b.pos = Vec2 { x: 1.0, y: 2.0 };
        let d = BotState::diff_json(&a, &b).unwrap();
        assert_eq!(d.as_object().unwrap().len(), 2);
        assert!(d.get("pos").is_some());
    }

    #[test]
    fn bot_array_added_removed_changed() {
        let mut a = BotArray::new();
        a.add(); // 0
        a.add(); // 1
        a.add(); // 2

        // removal + in-place change
        let mut b = a.clone();
        b.remove(1);
        b.arr[0].health = 4.0;

        let d = BotArray::diff_json(&a, &b).unwrap();
        assert_eq!(d["removed"], json!([1]));
        assert_eq!(d["changed"], json!({ "0": { "health": 4.0 } }));
        assert!(d.get("added").is_none());

        // addition: `add` fills the lowest free slot, which is 3 here
        let mut b = a.clone();
        assert_eq!(b.add(), 3);
        b.arr[3].health = 3.0;

        let d = BotArray::diff_json(&a, &b).unwrap();
        assert_eq!(d.as_object().unwrap().len(), 1);
        assert_eq!(d["added"]["3"]["health"], json!(3.0));
        assert_eq!(d["added"]["3"]["id"], json!(3));
    }

    #[test]
    fn bot_array_omits_empty_sections() {
        let mut a = BotArray::new();
        a.add();
        let mut b = a.clone();
        b.arr[0].angle = 90.0;

        let d = BotArray::diff_json(&a, &b).unwrap();
        assert_eq!(d.as_object().unwrap().len(), 1);
        assert!(d.get("changed").is_some());
    }

    /// A dead slot still holds whatever the bot that occupied it left behind.
    /// That data must never surface in a diff.
    #[test]
    fn bot_array_ignores_ghost_slots() {
        let a = BotArray::new();
        let mut b = a.clone();
        b.arr[5] = bot(5);
        b.arr[5].health = 999.0;

        assert!(BotArray::diff_json(&a, &b).is_none());
    }

    // Test-only types, so the `always` checks don't depend on changing the real
    // gamelog wire format.
    #[derive(serde::Serialize, PartialEq, Clone, Diff)]
    struct Stamped {
        #[diff(always)]
        id: u32,
        health: f32,
    }

    #[derive(serde::Serialize, PartialEq, Clone, Diff)]
    struct OnlyAlways {
        #[diff(always)]
        id: u32,
        #[diff(skip)]
        scratch: u32,
    }

    #[test]
    fn always_field_survives_when_unchanged() {
        let a = Stamped { id: 7, health: 10.0 };
        let mut b = a.clone();
        b.health = 7.5;

        // `id` did not change, but is emitted anyway to stamp the diff
        let d = Stamped::diff_json(&a, &b).unwrap();
        assert_eq!(d, json!({ "id": 7, "health": 7.5 }));
    }

    /// `always` must not defeat the `a == b` early return -- otherwise every
    /// unchanged bot would land in `BotArray`'s `changed` map every tick.
    #[test]
    fn always_field_does_not_resurrect_equal_values() {
        let a = Stamped { id: 7, health: 10.0 };
        assert!(Stamped::diff_json(&a, &a.clone()).is_none());
    }

    /// An `always` field counts as content: it alone is enough to emit, even
    /// when the only field that actually changed is skipped.
    #[test]
    fn always_field_alone_is_emitted() {
        let a = OnlyAlways { id: 7, scratch: 0 };
        let mut b = a.clone();
        b.scratch = 1;

        let d = OnlyAlways::diff_json(&a, &b).unwrap();
        assert_eq!(d, json!({ "id": 7 }));
    }

    #[test]
    fn game_state_recurses_into_fleets() {
        let a = GameState::new(&conf());

        // `tick` is a leaf, so advancing it alone is a one-field diff
        let mut b = a.clone();
        b.tick = 1;
        let d = GameState::diff_json(&a, &b).unwrap();
        assert_eq!(d, json!({ "tick": 1 }));

        let mut b = a.clone();
        b.fleet_a.add();
        let d = GameState::diff_json(&a, &b).unwrap();
        assert_eq!(d.as_object().unwrap().len(), 1);
        assert!(d["fleet_a"]["added"]["0"].is_object());
    }
}
