use serde::{ Serialize, Deserialize };
use super::util::Vec2;
use super::config::*;
use std::ops::{ Index, IndexMut };

type PlayerId = u32;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8, C)]
pub enum StateOption<T> {
    None    = 0,
    Some(T) = 1
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
            StateOption::None => None
        }
    }
}

impl<T> From<Option<T>> for StateOption<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(t) => StateOption::Some(t),
            None    => StateOption::None
        }
    }
}

impl<T> StateOption<T> {
    pub fn option(self) -> Option<T> {
        match self {
            StateOption::None => None,
            StateOption::Some(t) => Some(t)
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Team{
    A,
    B
}

impl Team {
    pub fn other(&self) -> Team {
        match self {
            Team::A => Team::B,
            Team::B => Team::A,
        }
    }
}

impl Mirror for Team {
    fn mirror(&mut self, _: &GameConfig) {
        *self = self.other();
    }
}


#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct TeamPair<T> {
    pub a: T,
    pub b: T,
}

impl<T> TeamPair<T> {
    pub fn new(a: T, b: T) -> Self {
        Self{ a, b }
    }
}

impl<T> Index<Team> for TeamPair<T> {
    type Output = T;
    fn index(&self, index: Team) -> &Self::Output {
        match index {
            Team::A => &self.a,
            Team::B => &self.b
        }
    }
}

impl<T> IndexMut<Team> for TeamPair<T> {
    fn index_mut(&mut self, index: Team) -> &mut Self::Output {
        match index {
            Team::A => &mut self.a,
            Team::B => &mut self.b
        }
    }
}

impl<T> IntoIterator for TeamPair<T> {
    type Item = T;
    type IntoIter = std::array::IntoIter<T, 2>;

    fn into_iter(self) -> Self::IntoIter {
        [self.a, self.b].into_iter()
    }
}

impl<'a, T> IntoIterator for &'a TeamPair<T> {
    type Item = &'a T;
    type IntoIter = std::array::IntoIter<&'a T, 2>;

    fn into_iter(self) -> Self::IntoIter {
        [&self.a, &self.b].into_iter()
    }
}

impl<'a, T> IntoIterator for &'a mut TeamPair<T> {
    type Item = &'a mut T;
    type IntoIter = std::array::IntoIter<&'a mut T, 2>;

    fn into_iter(self) -> Self::IntoIter {
        [&mut self.a, &mut self.b].into_iter()
    }
}

impl<T> TeamPair<T> {
    pub fn iter(&self) -> std::array::IntoIter<&T, 2> {
        [&self.a, &self.b].into_iter()
    }

    pub fn iter_mut(&mut self) -> std::array::IntoIter<&mut T, 2> {
        [&mut self.a, &mut self.b].into_iter()
    }
}

impl<T> Index<Team> for PlayerArray<T> {
    type Output = [T];
    
    fn index(&self, team: Team) -> &Self::Output {
        match team {
            Team::A => &self[..(NUM_PLAYERS as usize)],
            Team::B => &self[(NUM_PLAYERS as usize)..]
        }
    }
}

impl<T> Mirror for TeamPair<T> where T: Mirror {
    fn mirror(&mut self, conf: &GameConfig) {
        std::mem::swap(&mut self.a, &mut self.b);
        self.a.mirror(conf);
        self.b.mirror(conf);
    }
}

impl<T> IndexMut<Team> for PlayerArray<T> {
    fn index_mut(&mut self, team: Team) -> &mut Self::Output {
        match team {
            Team::A => &mut self[..(NUM_PLAYERS as usize)],
            Team::B => &mut self[(NUM_PLAYERS as usize)..]
        }
    }
}

pub trait Mirror {
    fn mirror(&mut self, conf: &GameConfig);
}

impl<T> Mirror for PlayerArray<T> where T: Mirror {
    fn mirror(&mut self, conf: &GameConfig) {
        self.rotate_left(NUM_PLAYERS as usize);
        self.iter_mut().for_each(|it| it.mirror(conf));
    }
}

impl<T> Mirror for [T; NUM_PLAYERS as usize] where T: Mirror {
    fn mirror(&mut self, conf: &GameConfig) {
        self.iter_mut().for_each(|it| it.mirror(conf));
    }
}

pub fn mirror_pos(pos: &mut Vec2, conf: &GameConfig) {
    pos.x = conf.field.width as f32 - pos.x;
}

pub fn mirror_player_id(id: &mut PlayerId) {
    if *id < NUM_PLAYERS {
        *id += NUM_PLAYERS;
    } else {
        *id -= NUM_PLAYERS;
    }
}

impl Mirror for Vec2 {
    fn mirror(&mut self, _: &GameConfig) {
        self.x *= -1.0;
    }
}


#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct PlayerState {
    pub id: PlayerId,
    pub pos: Vec2,
    pub dir: Vec2,
    pub speed: f32,
    pub radius: f32,
    pub pickup_radius: f32,
}

impl Mirror for PlayerState {
    fn mirror(&mut self, conf: &GameConfig) {
        mirror_player_id(&mut self.id);
        mirror_pos(&mut self.pos, conf);
        self.dir.mirror(conf);
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[repr(C)]
pub struct PlayerAction {
    pub dir: Vec2,
    pub pass: StateOption<Vec2>,
}

impl PlayerAction {
    pub fn sanitize(&mut self) {
        if !self.dir.x.is_finite() { self.dir.x = 0.0; }
        if !self.dir.y.is_finite() { self.dir.y = 0.0; }
        if let StateOption::Some(pass) = &mut self.pass {
            if !pass.x.is_finite() { pass.x = 0.0; }
            if !pass.y.is_finite() { pass.y = 0.0; }
        }
    }
}

impl Mirror for PlayerAction {
    fn mirror(&mut self, conf: &GameConfig) {
        self.dir.mirror(conf);
        if let StateOption::Some(ref mut pass) = self.pass {
            pass.mirror(conf);
        }
    }
}

impl Mirror for u32 {
    fn mirror(&mut self, _: &GameConfig) { }
}

pub type TeamAction = [PlayerAction; NUM_PLAYERS as usize];
pub type PlayerArray<T> = [T; NUM_PLAYERS as usize * 2];

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(u8, C)]
pub enum BallPossessionState {
    Possessed {
        owner: PlayerId,
        team: Team,
        capture_ticks: u32,
    }, 
    Passing { team: Team },
    Free
}

impl Mirror for BallPossessionState {
    fn mirror(&mut self, conf: &GameConfig) {
        use BallPossessionState::*;
        match self {
            Possessed { owner, team, .. } => {
                mirror_player_id(owner);
                team.mirror(conf);
            },
            Passing { team } => {
                team.mirror(conf);
            },
            _ => ()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct BallStagnationState {
    pub center: Vec2,
    pub tick: u32,
}

impl Mirror for BallStagnationState {
    fn mirror(&mut self, conf: &GameConfig) {
        mirror_pos(&mut self.center, conf);
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct BallState {
    pub pos: Vec2,
    pub vel: Vec2,
    pub radius: f32,
}

impl Mirror for BallState {
    fn mirror(&mut self, conf: &GameConfig) {
        mirror_pos(&mut self.pos, conf);
        self.vel.mirror(conf);
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct GameState {
    pub tick: u32,
    pub ball: BallState,
    pub ball_possession: BallPossessionState,
    pub ball_stagnation: BallStagnationState,
    pub players: PlayerArray<PlayerState>,
    pub score: TeamPair<u32>
}

impl Mirror for GameState {
    fn mirror(&mut self, conf: &GameConfig) {
        self.ball.mirror(conf);
        self.ball_possession.mirror(conf);
        self.ball_stagnation.mirror(conf);
        self.players.mirror(conf);
        self.score.mirror(conf);
    }
}

impl GameState {
 
    pub fn new(conf: &GameConfig) -> Self {
        let center = conf.field.center();
        GameState {
            tick: 0,
            ball: BallState {
                pos: center,
                vel: Vec2::ZERO,
                radius: conf.ball.radius,
            },
            ball_possession: BallPossessionState::Free,
            ball_stagnation: BallStagnationState {
                center,
                tick: 0
            },
            players: std::array::from_fn(|i| PlayerState {
                id: i as u32,
                pos: center,
                dir: Vec2::ZERO,
                speed: conf.player.speed,
                radius: conf.player.radius,
                pickup_radius: conf.player.pickup_radius
            }),
            score: TeamPair { a: 0, b: 0 }
        }
    }

    #[inline(always)]
    pub fn is_ball_free(&self) -> bool {
        matches!(self.ball_possession, BallPossessionState::Free)
    }

    #[inline(always)]
    pub fn ball_owner(&self) -> Option<PlayerId> {
        if let BallPossessionState::Possessed { owner, .. } = self.ball_possession {
            Some(owner)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn player_team(&self, id: PlayerId) -> Option<Team> {
        if id < NUM_PLAYERS {
            Some(Team::A)
        } else if id < NUM_PLAYERS * 2 {
            Some(Team::B)
        } else {
            None
        }
    }

    pub fn teams<'a>(&'a self) -> TeamPair<&'a [PlayerState]> {
        let (a, b) = self.players.split_at(NUM_PLAYERS as usize);
        TeamPair { a, b }
    }
    
    pub fn teams_mut<'a>(&'a mut self) -> TeamPair<&'a mut [PlayerState]> {
        let (a, b) = self.players.split_at_mut(NUM_PLAYERS as usize);
        TeamPair { a, b }
    }
}

