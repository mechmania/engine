#[cfg(feature = "engine")]
use crate::game::diff::Diff;
use crate::game::team::{Team, TeamPair};

use super::config::*;
use super::util::{normalize_degrees, Vec2};
use serde::{Deserialize, Serialize};
use std::ops::{Index, IndexMut};

pub type BotId = u8;


// NOTE the rust compiler sometimes does some optimization with the standard option enum
// for example, if a datatype is invalid when the memory is all 0's, the compiler will use
// this value in memory as the None variant rather than having an explicit discriminant in memory.
// to work around this, we define our own silly enum so the compiler does not do this
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, mm_macros::FfiMirror)]
#[repr(u8, C)]
pub enum StateOption<T> {
    None = 0,
    Some(T) = 1,
}

impl<T> Default for StateOption<T> {
    fn default() -> Self {
        StateOption::None
    }
}

impl<T> From<StateOption<T>> for Option<T> {
    fn from(value: StateOption<T>) -> Self {
        match value {
            StateOption::Some(t) => Some(t),
            StateOption::None => None,
        }
    }
}

impl<T> From<Option<T>> for StateOption<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(t) => StateOption::Some(t),
            None => StateOption::None,
        }
    }
}

impl<T> StateOption<T> {
    pub fn option(self) -> Option<T> {
        match self {
            StateOption::None => None,
            StateOption::Some(t) => Some(t),
        }
    }
}

pub trait Mirror {
    fn mirror(&mut self, conf: &GameConfig);
}

impl<T> Mirror for [T; BOTS_MAX as usize]
where
    T: Mirror,
{
    fn mirror(&mut self, conf: &GameConfig) {
        self.iter_mut().for_each(|it| it.mirror(conf));
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[cfg_attr(feature = "engine", derive(Diff))]
#[repr(C)]
pub struct BotState {
    pub id: BotId,
    pub health: f32,
    pub pos: Vec2,
    pub vel: Vec2,
    pub angle: f32,
    pub turn_vel: f32,
    /// Absolute tick at which this bot stops being invulnerable. `0` means vulnerable now.
    /// Set when a blast lands; while it is in the future the bot takes no blaster damage,
    /// which is also what limits a bot to one blast per tick. Absolute rather than a
    /// countdown, which would dirty the diff of every hurt bot each tick. Flat rather than
    /// part of `special` because taking a blast is something every class does.
    pub invulnerable_until_tick: u32,
    /// Everything that exists only for this bot's class, and the class itself -- see
    /// `SpecialState`.
    pub special: SpecialState,
}

impl BotState {
    pub fn class(&self) -> BotClass {
        self.special.class()
    }

    /// The blaster's readiness tick, or `0` -- "ready now" -- for a class that has no
    /// blaster. Classes without one never reach the cooldown check, so the value they
    /// report is arbitrary; `0` keeps `bot.next_fire_tick() <= tick` reading naturally.
    pub fn next_fire_tick(&self) -> u32 {
        match self.special {
            SpecialState::Battle { next_fire_tick, .. } => next_fire_tick,
            _ => 0,
        }
    }

    /// Impact point of this tick's shot, `None` on any tick this bot did not fire and on
    /// any class that cannot.
    pub fn shot(&self) -> StateOption<Vec2> {
        match self.special {
            SpecialState::Battle { shot, .. } => shot,
            _ => StateOption::None,
        }
    }

    /// The ally this bot healed this tick, `None` on any tick it did not and on any class
    /// that cannot heal.
    pub fn healing(&self) -> StateOption<BotId> {
        match self.special {
            SpecialState::Healer { healing } => healing,
            _ => StateOption::None,
        }
    }

    /// The deposit this bot extracted from this tick, named by the team that owns it.
    /// `None` on any tick it held no extraction slot and on any class that cannot extract.
    pub fn extracting(&self) -> StateOption<Team> {
        match self.special {
            SpecialState::Extractor { extracting } => extracting,
            _ => StateOption::None,
        }
    }
}

impl Mirror for BotState {
    fn mirror(&mut self, conf: &GameConfig) {
        mirror_pos(&mut self.pos);
        mirror_vel(&mut self.vel);
        // The world is rotated 180 degrees, so the bot's facing turns with it. `turn_vel` is
        // a signed rotation *rate*, which a rotation preserves -- it stays as-is.
        self.angle = normalize_degrees(self.angle - 180.0);
        self.special.mirror(conf);
        // `invulnerable_until_tick` is an absolute tick, so it is side-agnostic already.
    }
}

/// The part of a bot's state that exists only for its class, and the record of what its
/// special did this tick. The mirror image of `SpecialAction`, and the reason `BotState`
/// carries no `class` field: the variant *is* the class, so the two cannot drift apart.
///
/// A `Diff` **leaf**, deliberately and necessarily. `#[derive(Diff)]` rejects enums outright
/// (see `mm-macros/src/diff.rs`), and the gamelog consumer merges a changed field by
/// replacing it whole, so a partially-emitted variant would silently lose its other fields.
/// A changed `special` therefore costs the whole variant in the diff line -- a few bytes on
/// the ticks a bot fires, and nothing on any other.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[repr(u8, C)]
pub enum SpecialState {
    Battle {
        /// Absolute tick at which the blaster is next ready. `0` means ready now. Absolute
        /// rather than a countdown so it only changes on the tick a shot is fired -- a
        /// per-tick counter would put every bot on cooldown into every gamelog diff.
        next_fire_tick: u32,
        /// Impact point of this tick's shot, `None` on any tick this bot did not fire.
        /// Cleared at the top of `step_blasters`, which is what makes it transient: the
        /// `Some` -> `None` transition is itself a diff, so the gamelog self-clears.
        shot: StateOption<Vec2>,
    },
    Healer {
        /// The ally this healer's channel actually reached this tick -- set only once every
        /// gate in `step_healers` has passed, so it marks a heal that landed, not one that
        /// was merely asked for. A healer may name itself.
        ///
        /// Cleared per tick like `shot`, but unlike `shot` a steady channel costs the
        /// gamelog nothing: the clear and the re-set both happen inside one tick, so a diff
        /// against the previous tick sees `Some(t)` unchanged. Only the start and the end of
        /// a channel emit.
        healing: StateOption<BotId>,
    },
    Extractor {
        /// The deposit this extractor is actually drawing from -- set only once every gate
        /// in `step_extractors` has passed *and* it has won one of that deposit's slots, so
        /// it marks extraction that landed, not extraction that was merely asked for.
        ///
        /// A deposit is named by the team it belongs to, because there is exactly one per
        /// team. Cleared per tick like `healing`, and like `healing` a steady channel costs
        /// the gamelog nothing -- the clear and the re-set both happen inside one tick.
        extracting: StateOption<Team>,
    },
}

impl SpecialState {
    pub fn class(&self) -> BotClass {
        match self {
            SpecialState::Battle { .. } => BotClass::Battle,
            SpecialState::Healer { .. } => BotClass::Healer,
            SpecialState::Extractor { .. } => BotClass::Extractor,
        }
    }

    /// The starting state for a freshly built bot of `class`.
    pub fn new(class: BotClass) -> Self {
        match class {
            BotClass::Battle => SpecialState::Battle {
                next_fire_tick: 0,
                shot: StateOption::None,
            },
            BotClass::Healer => SpecialState::Healer {
                healing: StateOption::None,
            },
            BotClass::Extractor => SpecialState::Extractor {
                extracting: StateOption::None,
            },
        }
    }
}

impl Default for SpecialState {
    fn default() -> Self {
        Self::new(BotClass::Battle)
    }
}

impl Mirror for SpecialState {
    fn mirror(&mut self, conf: &GameConfig) {
        match self {
            // A shot is a point in the world, so it rotates with everything else.
            SpecialState::Battle { shot, .. } => {
                if let StateOption::Some(point) = shot {
                    mirror_pos(point);
                }
            }
            // `BotId` is fleet-local, and a healer only ever names its own fleet, so a heal
            // target survives the swap of the two fleets untouched.
            SpecialState::Healer { .. } => {}
            // A deposit is named by the team that owns it, and the two deposits swap places
            // with the two fleets -- see `GameState::mirror`. So the name flips with the
            // team it names.
            SpecialState::Extractor { extracting } => {
                if let StateOption::Some(team) = extracting {
                    team.mirror(conf);
                }
            }
        }
    }
}

pub trait Action {
    fn sanitize(&mut self);
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[repr(C)]
pub struct MoveAction {
    pub direction: Vec2,
}

impl Default for MoveAction {
    fn default() -> Self {
        MoveAction {
            direction: Vec2::default(),
        }
    }
}

impl Action for MoveAction {
    fn sanitize(&mut self) {
        self.direction = self.direction.normalize_or_zero();
    }
}

impl Mirror for MoveAction {
    fn mirror(&mut self, _conf: &GameConfig) {
        mirror_vel(&mut self.direction);
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[repr(u8, C)]
pub enum TurnAction {
    Direction { power: f32 }, // capped at 1.0 magnitude
    TargetRotation { deg: f32 },
    TargetPosition { pos: Vec2 }, // pointing towards target position
}

impl Default for TurnAction {
    fn default() -> Self {
        Self::Direction { power: 0.0 }
    }
}

impl Action for TurnAction {
    fn sanitize(&mut self) {
        match self {
            Self::Direction { power: rot_vel } => {
                *rot_vel = rot_vel.clamp(-1.0, 1.0);
            }
            Self::TargetRotation { deg } => {
                *deg = normalize_degrees(*deg);
            }
            _ => (),
        }
    }
}

impl Mirror for TurnAction {
    fn mirror(&mut self, _conf: &GameConfig) {
        match self {
            // `power` is a signed turn *rate*, and a 180 degree rotation preserves the
            // sense of rotation, so this is side-agnostic already.
            Self::Direction { .. } => {}
            Self::TargetRotation { deg } => {
                *deg -= 180.0;
                self.sanitize();
            }
            Self::TargetPosition { pos } => {
                mirror_pos(pos);
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[repr(u8, C)]
pub enum SpecialAction {
    // TODO work these out
    Battle { fire: bool },
    Healer { fire: bool, target: BotId },
    Extractor { mine: bool },
}

impl SpecialAction {
    pub fn class(&self) -> BotClass {
        match self {
            SpecialAction::Battle { .. } => BotClass::Battle,
            SpecialAction::Healer { .. } => BotClass::Healer,
            SpecialAction::Extractor { .. } => BotClass::Extractor,
        }
    }
}

impl Default for SpecialAction {
    fn default() -> Self {
        Self::Battle { fire: false }
    }
}

impl Mirror for SpecialAction {
    fn mirror(&mut self, _conf: &GameConfig) {} // NOTE for now these are side agnostic
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug, mm_macros::FfiMirror)]
#[repr(C)]
pub struct BotAction {
    pub move_action: MoveAction,
    pub turn_action: TurnAction,
    pub special_action: SpecialAction, // TODO
    pub self_destruct: bool,
}

impl Mirror for BotAction {
    fn mirror(&mut self, conf: &GameConfig) {
        self.move_action.mirror(conf);
        self.turn_action.mirror(conf);
        self.special_action.mirror(conf);
    }
}

impl Action for BotAction {
    fn sanitize(&mut self) {
        self.move_action.sanitize();
        self.turn_action.sanitize();
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default, mm_macros::FfiMirror)]
#[repr(u8)]
pub enum BotClass {
    #[default]
    Battle,
    Healer,    // TODO
    Extractor, // TODO
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug, mm_macros::FfiMirror)]
#[repr(C)]
pub struct FleetAction {
    pub bots: [BotAction; BOTS_MAX],
    /// The class the fabricator builds next -- for the natural build *and* for a rush
    /// order, which is the same build paid for early.
    pub fabricator_next: BotClass,
    /// Buy a bot on the spot for `conf.fabricator.rush_cost`, independently of
    /// `next_bot_creation`. Refused, and not charged, in the endgame.
    pub rush_order: bool,
}

impl FleetAction {
    /// The do-nothing action: no movement, no specials, no rush order, and the fabricator
    /// building `BotClass::default()` on its own cadence. Identical to `Default::default()`
    /// -- kept as the name the bot crates and the tests construct through, so what an
    /// "empty" action means stays in one place if a field ever needs a non-zero default.
    pub fn new() -> Self {
        Default::default()
    }
}

impl Mirror for FleetAction {
    fn mirror(&mut self, conf: &GameConfig) {
        self.bots
            .iter_mut()
            .for_each(|bot_action| bot_action.mirror(conf));
        // `fabricator_next` is a `BotClass` and `rush_order` is a `bool`. Neither carries a
        // position, a direction or a team, so both are invariant
        // under the 180 degree rotation -- nothing to do here, and nothing to reindex: the
        // fabricator is per-fleet, not per-bot, so the fleet swap in `GameState::mirror`
        // already puts it with the right team.
    }
}

impl Action for FleetAction {
    fn sanitize(&mut self) {
        self.bots
            .iter_mut()
            .for_each(|bot_action| bot_action.sanitize());
    }
}

#[derive(Clone, PartialEq, Debug, mm_macros::FfiMirror)]
#[repr(C)]
pub struct BotArray {
    pub len: u8,
    pub mask: [bool; BOTS_MAX],
    pub arr: [BotState; BOTS_MAX],
}


impl BotArray {
    pub fn new() -> Self {
        BotArray {
            len: 0,
            mask: [false; BOTS_MAX],
            arr: std::array::from_fn(|i: usize| BotState {
                id: i as u8,
                health: 0.0,
                pos: Vec2::ZERO,
                vel: Vec2::ZERO,
                angle: 0.0,
                turn_vel: 0.0,
                invulnerable_until_tick: 0,
                special: SpecialState::default(),
            }),
        }
    }

    // panics on invalid inputs
    pub fn remove(&mut self, id: BotId) {
        let id = id as usize;
        assert!(self.mask[id]);
        self.len -= 1;
        self.mask[id] = false;
    }

    // panics on full array
    pub fn add(&mut self) -> BotId {
        let id = self.mask.iter().position(|bit| !bit).expect("cannot add new bot to full array");
        self.mask[id] = true;
        self.len += 1;
        id as BotId
    }

    pub fn iter<'a>(&'a self) -> BotArrayIter<'a> {
        BotArrayIter {
            bot_slice: &self.arr,
            mask_slice: &self.mask,
        }
    }

    pub fn iter_mut<'a>(&'a mut self) -> BotArrayIterMut<'a> {
        BotArrayIterMut {
            bot_slice: &mut self.arr,
            mask_slice: &mut self.mask,
        }
    }

    pub fn get<'a>(&'a self, id: BotId) -> Option<&'a BotState> {
        let id = id as usize;
        return if id < BOTS_MAX && self.mask[id] {
            Some(&self.arr[id])
        } else {
            None
        };
    }

    pub fn get_mut<'a>(&'a mut self, id: BotId) -> Option<&'a mut BotState> {
        let id = id as usize;
        return if id < BOTS_MAX && self.mask[id] {
            Some(&mut self.arr[id])
        } else {
            None
        };
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.len as usize == BOTS_MAX
    }
}

impl Serialize for BotArray {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        self.iter().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BotArray {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de> {
        let vec = Vec::<BotState>::deserialize(deserializer)?;
        let mut res = BotArray::new();
        res.len = vec.len() as u8;
        for bot in vec {
            res[bot.id] = bot.clone();
            res.mask[bot.id as usize] = true;
        }
        Ok(res)
    }
}

impl Index<BotId> for BotArray {
    type Output = BotState;

    fn index(&self, id: BotId) -> &Self::Output {
        let id = id as usize;
        if id >= BOTS_MAX || !self.mask[id] {
            panic!("attempted to access invalid bot id: {}", id);
        }
        &self.arr[id]
    }
}

impl IndexMut<BotId> for BotArray {
    fn index_mut(&mut self, id: BotId) -> &mut Self::Output {
        let id = id as usize;
        if id >= BOTS_MAX || !self.mask[id] {
            panic!("attempted to access invalid bot id: {}", id);
        }
        &mut self.arr[id]
    }
}

pub struct BotArrayIter<'a> {
    bot_slice: &'a [BotState],
    mask_slice: &'a [bool],
}

impl<'a> Iterator for BotArrayIter<'a> {
    type Item = &'a BotState;

    fn next(&mut self) -> Option<Self::Item> {
        let bot_slice = std::mem::take(&mut self.bot_slice);
        let mask_slice = std::mem::take(&mut self.mask_slice);

        if let Some(pos) = mask_slice.iter().position(|b| *b) {
            let (bot_head, bot_tail) = bot_slice.split_at(pos + 1);
            let (_, mask_tail) = mask_slice.split_at(pos + 1);
            self.bot_slice = bot_tail;
            self.mask_slice = mask_tail;

            bot_head.last()
        } else {
            return None;
        }
    }
}

pub struct BotArrayIterMut<'a> {
    bot_slice: &'a mut [BotState],
    mask_slice: &'a [bool],
}

impl<'a> Iterator for BotArrayIterMut<'a> {
    type Item = &'a mut BotState;

    fn next(&mut self) -> Option<Self::Item> {
        let bot_slice = std::mem::take(&mut self.bot_slice);
        let mask_slice = std::mem::take(&mut self.mask_slice);

        if let Some(pos) = mask_slice.iter().position(|b| *b) {
            let (bot_head, bot_tail) = bot_slice.split_at_mut(pos + 1);
            let (_, mask_tail) = mask_slice.split_at(pos + 1);
            self.bot_slice = bot_tail;
            self.mask_slice = mask_tail;

            bot_head.last_mut()
        } else {
            return None;
        }
    }
}

impl Mirror for BotArray {
    fn mirror(&mut self, conf: &GameConfig) {
        for bot in self.iter_mut() {
            bot.mirror(conf);
        }
    }
}

/// The mutable half of a deposit: where it is, and who is currently drawing from it. The
/// static half -- radius and the slot cap -- is `config::DepositConfig`, shared by both.
///
/// There is exactly one deposit per team, which is what lets a bot name one with a bare
/// `Team` (see `SpecialState::Extractor`). Ownership is a label only: either team may
/// extract from either deposit, on identical terms.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default, mm_macros::FfiMirror)]
#[cfg_attr(feature = "engine", derive(Diff))]
#[repr(C)]
pub struct Deposit {
    pub pos: Vec2,
    /// Bit `i` is set when bot `i` of that team holds one of this deposit's
    /// `conf.deposit.extractor_cap` slots. `BOTS_MAX == 32`, so one `u32` covers a whole
    /// fleet: membership is a bit test, admission an `|=`, and occupancy a `count_ones()`.
    /// A `Diff` leaf -- two integers, and they only move when membership actually changes.
    pub extractors: TeamPair<u32>,
}

impl Mirror for Deposit {
    fn mirror(&mut self, _conf: &GameConfig) {
        mirror_pos(&mut self.pos);
        // `TeamPair<T>: Mirror` wants `T: Mirror`, which `u32` is not -- and a bitmask of
        // fleet-local `BotId`s needs no per-element work anyway, only the swap.
        self.extractors.swap();
    }
}

/// Per-fleet economy and build queue: what the fleet has earned and when its next bot arrives.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default, mm_macros::FfiMirror)]
#[cfg_attr(feature = "engine", derive(Diff))]
#[repr(C)]
pub struct FabricatorState {
    /// Fleet currency, earned by extractors at `conf.bot.extract_rate` per extractor per
    /// tick and spent on rush orders -- see `action::step_fabricators`. Starts at
    /// `conf.fabricator.starting_tokens`.
    pub tokens: f32,
    /// Absolute tick of the next natural build. Absolute rather than a countdown for the
    /// same reason as `BotState::next_fire_tick`: a countdown would put both fabricators
    /// into every gamelog line.
    ///
    /// May sit in the *past*. A fleet already at `BOTS_MAX` cannot take its build, so the
    /// timer simply holds and the bot arrives the tick a slot opens, rather than the build
    /// being lost for being full.
    pub next_bot_creation: u32,
}

impl Mirror for FabricatorState {
    fn mirror(&mut self, _conf: &GameConfig) {
        // `tokens` is a scalar and `next_bot_creation` an absolute tick -- neither has a
        // side. The impl exists so that a field which
        // *does* have a side cannot be added without this line failing to be enough.
    }
}

// Team naming is rewritten per build -- see `mm_macros::teams` and `game::team`.
#[cfg_attr(feature = "engine", mm_macros::teams(A, B))]
#[cfg_attr(feature = "client", mm_macros::teams(Me, Other))]
mod game_state_impl {
    #[derive(Serialize, Deserialize, Clone, PartialEq, mm_macros::FfiMirror)]
    #[cfg_attr(feature = "engine", derive(Diff))]
    #[repr(C)]
    pub struct GameState {
        pub tick: u32,
        pub capture: f32,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fleet_team_a: BotArray,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fleet_team_b: BotArray,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub deposit_team_a: Deposit,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub deposit_team_b: Deposit,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fabricator_team_a: FabricatorState,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fabricator_team_b: FabricatorState,
    }

    impl Mirror for GameState {
        fn mirror(&mut self, conf: &GameConfig) {
            self.fleet_team_a.mirror(conf);
            self.fleet_team_b.mirror(conf);
            std::mem::swap(&mut self.fleet_team_a, &mut self.fleet_team_b);
            self.deposit_team_a.mirror(conf);
            self.deposit_team_b.mirror(conf);
            std::mem::swap(&mut self.deposit_team_a, &mut self.deposit_team_b);
            self.fabricator_team_a.mirror(conf);
            self.fabricator_team_b.mirror(conf);
            std::mem::swap(&mut self.fabricator_team_a, &mut self.fabricator_team_b);
            self.capture *= -1.0;
            // The map is not here to mirror: it lives in `GameConfig`, and it is invariant
            // under this rotation by construction -- see `config::MAP` and
            // `config::build_map`, which builds it from half a layout plus its rotation.
        }
    }

    impl GameState {
        pub fn new(conf: &GameConfig) -> Self {
            // Only team A's deposit is configured; team B's is its image, the same way only
            // half of `MAP_ART` and half of `PAYLOAD_PATH` are written.
            let mut mirrored = conf.deposit.pos;
            mirror_pos(&mut mirrored);
            Self {
                tick: 0,
                capture: 0.0,
                fleet_team_a: BotArray::new(),
                fleet_team_b: BotArray::new(),
                deposit_team_a: Deposit {
                    pos: conf.deposit.pos,
                    extractors: TeamPair::new(0, 0),
                },
                deposit_team_b: Deposit {
                    pos: mirrored,
                    extractors: TeamPair::new(0, 0),
                },
                // `next_bot_creation: 0` -- both fleets get their first bot on tick 0.
                fabricator_team_a: FabricatorState {
                    tokens: conf.fabricator.starting_tokens,
                    next_bot_creation: 0,
                },
                fabricator_team_b: FabricatorState {
                    tokens: conf.fabricator.starting_tokens,
                    next_bot_creation: 0,
                },
            }
        }

        /// Whether the match is in its endgame, the last `conf.endgame_ticks` ticks, during
        /// which no bot is built.
        pub fn in_endgame(&self, conf: &GameConfig) -> bool {
            self.tick >= conf.max_ticks.saturating_sub(conf.endgame_ticks)
        }

        /// Total current health over `team`'s living bots -- the second endgame tiebreak.
        pub fn health_pool(&self, team: Team) -> f32 {
            self.fleets()[team].iter().map(|bot| bot.health).sum()
        }

        /// Center of the payload circle. Its radius is `conf.payload.radius`.
        pub fn payload_pos(&self) -> Vec2 {
            payload_position(self.capture)
        }

        pub fn fleets(&self) -> TeamPair<&BotArray> {
            TeamPair {
                team_a: &self.fleet_team_a,
                team_b: &self.fleet_team_b,
            }
        }

        pub fn fleets_mut(&mut self) -> TeamPair<&mut BotArray> {
            TeamPair {
                team_a: &mut self.fleet_team_a,
                team_b: &mut self.fleet_team_b,
            }
        }

        /// The two deposits, indexed by the team each belongs to.
        pub fn deposits(&self) -> TeamPair<&Deposit> {
            TeamPair {
                team_a: &self.deposit_team_a,
                team_b: &self.deposit_team_b,
            }
        }

        pub fn deposits_mut(&mut self) -> TeamPair<&mut Deposit> {
            TeamPair {
                team_a: &mut self.deposit_team_a,
                team_b: &mut self.deposit_team_b,
            }
        }

        pub fn fabricators(&self) -> TeamPair<&FabricatorState> {
            TeamPair {
                team_a: &self.fabricator_team_a,
                team_b: &self.fabricator_team_b,
            }
        }

        pub fn fabricators_mut(&mut self) -> TeamPair<&mut FabricatorState> {
            TeamPair {
                team_a: &mut self.fabricator_team_a,
                team_b: &mut self.fabricator_team_b,
            }
        }
    }
}

#[cfg(feature = "engine")]
#[cfg(test)]
mod state_test {
    use super::*;
    use crate::game::config::BOTS_MAX;

    fn ensure_valid_bot_array(bot_array: &BotArray) {
        let mut len = 0;
        for i in 0..BOTS_MAX {
            if !bot_array.mask[i] {
                continue;
            }

            let bot_id = bot_array.arr[i].id as usize;
            assert_eq!(bot_id, i);

            len += 1;
        }

        assert_eq!(bot_array.len, len);
    }

    #[test]
    fn test_bot_array() {
        let mut bot_array = BotArray::new();
        ensure_valid_bot_array(&bot_array);
        for _ in 0..3 {
            bot_array.add();
        }
        ensure_valid_bot_array(&bot_array);

        bot_array.remove(1);
        ensure_valid_bot_array(&bot_array);

        assert!(matches!(bot_array.get(1), None));
        assert!(matches!(bot_array.get(0), Some(_)));

        assert_eq!(bot_array.len, 2);
        assert_eq!(bot_array.add(), 1);
        assert_eq!(bot_array.len, 3);

        bot_array.remove(1);

        let valid_ids = bot_array.iter().map(|bot| bot.id).collect::<Vec<_>>();
        assert_eq!(valid_ids, vec![0, 2]);

        bot_array.remove(0);
        bot_array.remove(2);

        for _ in bot_array.iter() {
            panic!("iter is iterating empty bot array")
        }
    }
}

/// `Validate` is what stands between a hand-filled `FleetAction` and a `std::ptr::read` in
/// the engine's own process, so the cases here are the bytes a bot writing the mapping by
/// hand actually gets wrong: a tag the source never declared, and a `bool` that is neither
/// 0 nor 1. Both are invalid values in Rust's sense -- the moment one exists, so does UB --
/// which is why the check is on the bytes rather than on the value.
#[cfg(test)]
mod validate_test {
    use super::*;
    use crate::game::mirror::Validate;
    use std::mem::{offset_of, size_of};

    /// The raw image of a real, engine-built action -- the thing a legitimate bot sends.
    fn image() -> Vec<u8> {
        let action = FleetAction::default();
        let raw = unsafe {
            std::slice::from_raw_parts(
                &action as *const FleetAction as *const u8,
                size_of::<FleetAction>(),
            )
        };
        raw.to_vec()
    }

    /// Byte offset of bot 0's action within a `FleetAction`.
    fn bot0() -> usize {
        offset_of!(FleetAction, bots)
    }

    #[test]
    fn a_real_action_validates() {
        assert!(FleetAction::validate(&image()));

        // Not just the default: a populated action exercises every payload-carrying arm.
        let mut action = FleetAction::default();
        action.bots[0].turn_action = TurnAction::TargetPosition { pos: Vec2::new(1.0, 2.0) };
        action.bots[1].special_action = SpecialAction::Healer { fire: true, target: 3 };
        action.bots[2].self_destruct = true;
        action.fabricator_next = BotClass::Extractor;
        action.rush_order = true;
        let raw = unsafe {
            std::slice::from_raw_parts(
                &action as *const FleetAction as *const u8,
                size_of::<FleetAction>(),
            )
        };
        assert!(FleetAction::validate(raw));
    }

    #[test]
    fn an_undeclared_data_enum_tag_is_rejected() {
        let mut bytes = image();
        // `TurnAction` declares 0..=2.
        bytes[bot0() + offset_of!(BotAction, turn_action)] = 3;
        assert!(!FleetAction::validate(&bytes));
    }

    #[test]
    fn an_undeclared_unit_enum_tag_is_rejected() {
        let mut bytes = image();
        // `BotClass` declares 0..=2; this is the field that shrank to one byte.
        bytes[offset_of!(FleetAction, fabricator_next)] = 9;
        assert!(!FleetAction::validate(&bytes));

    }

    #[test]
    fn a_bool_that_is_not_zero_or_one_is_rejected() {
        let mut bytes = image();
        bytes[offset_of!(FleetAction, rush_order)] = 7;
        assert!(!FleetAction::validate(&bytes));

        // Inside a variant payload, which only the shadow layout knows where to find.
        let mut bytes = image();
        let special = bot0() + offset_of!(BotAction, special_action);
        bytes[special] = 1; // Healer
        bytes[special + 1] = 2; // its `fire`
        assert!(!FleetAction::validate(&bytes));

        let mut bytes = image();
        bytes[bot0() + offset_of!(BotAction, self_destruct)] = 2;
        assert!(!FleetAction::validate(&bytes));
    }

    #[test]
    fn every_slot_is_checked_not_just_the_first() {
        let stride = size_of::<BotAction>();
        for slot in [1usize, BOTS_MAX - 1] {
            let mut bytes = image();
            bytes[bot0() + slot * stride + offset_of!(BotAction, turn_action)] = 3;
            assert!(!FleetAction::validate(&bytes), "slot {slot} went unchecked");
        }
    }

    /// Floats are the other half of the contract: every bit pattern is a legal `f32`, so a
    /// NaN target or an absurd magnitude is a *game* question for `sanitize`, not a
    /// validity one. Rejecting it here would turn a bad move into a dropped tick.
    #[test]
    fn a_nonsense_float_is_still_a_valid_value() {
        let mut bytes = image();
        let dir = bot0() + offset_of!(BotAction, move_action) + offset_of!(MoveAction, direction);
        bytes[dir..dir + 4].copy_from_slice(&f32::NAN.to_ne_bytes());
        assert!(FleetAction::validate(&bytes));
    }
}
