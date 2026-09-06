#[cfg(feature = "engine")]
use crate::game::diff::Diff;
use crate::game::team::TeamPair;

use super::config::*;
use super::util::{normalize_degrees, Vec2};
use serde::{Deserialize, Serialize};
use std::ops::{Index, IndexMut};

pub type BotId = u8;


// NOTE the rust compiler sometimes does some optimization with the standard option enum
// for example, if a datatype is invalid when the memory is all 0's, the compiler will use
// this value in memory as the None variant rather than having an explicit discriminant in memory.
// to work around this, we define our own silly enum so the compiler does not do this
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[cfg_attr(feature = "engine", derive(Diff))]
#[repr(C)]
pub struct BotState {
    pub id: BotId,
    pub class: BotClass,
    pub health: f32,
    pub pos: Vec2,
    pub vel: Vec2,
    pub angle: f32,
    pub turn_vel: f32,
    /// Absolute tick at which the blaster is next ready. `0` means ready now. Absolute
    /// rather than a countdown so it only changes on the tick a shot is fired -- a
    /// per-tick counter would put every bot on cooldown into every gamelog diff.
    pub next_fire_tick: u32,
    /// Absolute tick at which this bot stops being invulnerable. `0` means vulnerable now.
    /// Set when a blast lands; while it is in the future the bot takes no blaster damage,
    /// which is also what limits a bot to one blast per tick. Absolute for the same reason
    /// as `next_fire_tick` -- a countdown would dirty the diff of every hurt bot each tick.
    pub invulnerable_until_tick: u32,
    /// Impact point of this tick's shot, `None` on any tick this bot did not fire.
    pub shot: StateOption<Vec2>,
}

impl Mirror for BotState {
    fn mirror(&mut self, _conf: &GameConfig) {
        mirror_pos(&mut self.pos);
        mirror_vel(&mut self.vel);
        // The world is rotated 180 degrees, so the bot's facing turns with it. `turn_vel` is
        // a signed rotation *rate*, which a rotation preserves -- it stays as-is.
        self.angle = normalize_degrees(self.angle - 180.0);
        if let StateOption::Some(point) = &mut self.shot {
            mirror_pos(point);
        }
        // `next_fire_tick` and `invulnerable_until_tick` are absolute ticks, so they are
        // side-agnostic already.
    }
}

pub trait Action {
    fn sanitize(&mut self);
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[repr(C)]
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[repr(C)]
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[repr(C)]
pub enum BotClass {
    #[default]
    Battle,
    Healer,    // TODO
    Extractor, // TODO
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[repr(C)]
pub struct FleetAction {
    pub bots: [BotAction; BOTS_MAX],
    pub fabricator_next: BotClass,
    pub fabricator_on: bool, // TODO upgrade
}

impl FleetAction {
    pub fn new() -> Self {
        let mut res: Self = Default::default();
        res.fabricator_on = true;
        res
    }
}

impl Mirror for FleetAction {
    fn mirror(&mut self, conf: &GameConfig) {
        self.bots
            .iter_mut()
            .for_each(|bot_action| bot_action.mirror(conf));
        // TODO upgrade
        // TODO fabricator
    }
}

impl Action for FleetAction {
    fn sanitize(&mut self) {
        self.bots
            .iter_mut()
            .for_each(|bot_action| bot_action.sanitize());
    }
}

#[derive(Clone, PartialEq, Debug)]
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
                class: BotClass::Battle,
                health: 0.0,
                pos: Vec2::ZERO,
                vel: Vec2::ZERO,
                angle: 0.0,
                turn_vel: 0.0,
                next_fire_tick: 0,
                invulnerable_until_tick: 0,
                shot: StateOption::None,
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

// Team naming is rewritten per build -- see `mm_macros::teams` and `game::team`.
#[cfg_attr(feature = "engine", mm_macros::teams(A, B))]
#[cfg_attr(feature = "client", mm_macros::teams(Me, Other))]
mod game_state_impl {
    #[derive(Serialize, Deserialize, Clone, PartialEq)]
    #[cfg_attr(feature = "engine", derive(Diff))]
    #[repr(C)]
    pub struct GameState {
        pub tick: u32,
        pub capture: f32,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fleet_team_a: BotArray,
        #[cfg_attr(feature = "engine", diff(nested))]
        pub fleet_team_b: BotArray,
    }

    impl Mirror for GameState {
        fn mirror(&mut self, conf: &GameConfig) {
            self.fleet_team_a.mirror(conf);
            self.fleet_team_b.mirror(conf);
            std::mem::swap(&mut self.fleet_team_a, &mut self.fleet_team_b);
            self.capture *= -1.0;
            // The map is not here to mirror: it lives in `GameConfig`, and it is invariant
            // under this rotation by construction -- see `config::MAP` and
            // `config::wall_test::the_map_is_mirror_symmetric`.
        }
    }

    impl GameState {
        pub fn new(_conf: &GameConfig) -> Self {
            Self {
                tick: 0,
                capture: 0.0,
                fleet_team_a: BotArray::new(),
                fleet_team_b: BotArray::new(),
            }
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
