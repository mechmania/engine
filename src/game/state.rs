use serde::{ Serialize, Deserialize };
use super::util::Vec2;
use super::config::*;
use std::ops::{ Index, IndexMut };

type PlayerId = u32;

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


#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
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

impl<T> IndexMut<Team> for PlayerArray<T> {
    fn index_mut(&mut self, team: Team) -> &mut Self::Output {
        match team {
            Team::A => &mut self[..(NUM_PLAYERS as usize)],
            Team::B => &mut self[(NUM_PLAYERS as usize)..]
        }
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

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct PlayerAction {
    pub dir: Vec2,
    pub pass: Option<Vec2>,
}

pub type TeamAction = [PlayerAction; NUM_PLAYERS as usize];
pub type PlayerArray<T> = [T; NUM_PLAYERS as usize * 2];

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub enum BallPossessionState {
    Possessed {
        owner: PlayerId,
        team: Team,
        capture_ticks: u32,
    }, 
    Passing { team: Team },
    Free
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct BallStagnationState {
    pub center: Vec2,
    pub ticks: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct BallState {
    pub pos: Vec2,
    pub vel: Vec2,
    pub radius: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct GameState {
    pub tick: u32,
    pub ball: BallState,
    pub ball_possession: BallPossessionState,
    pub ball_stagnation: BallStagnationState,
    pub players: PlayerArray<PlayerState>,
    // TODO goal owners, will they be used?
    pub score: TeamPair<u32>
}

impl GameState {
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

