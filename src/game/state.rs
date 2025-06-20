use serde::{ Serialize, Deserialize };
use super::util::Vec2;
use super::config::*;
use std::ops::{ Index, IndexMut };
use rand::{ seq::SliceRandom, thread_rng };

type PlayerId = u32;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Team{
    A,
    B
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct TeamPair<T> {
    pub a: T,
    pub b: T,
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

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct PlayerState {
    pub id: PlayerId,
    pub pos: Vec2,
    pub dir: Vec2,
    pub speed: f32,
    pub radius: f32,
    pub pickup_radius: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct PlayerAction {
    pub dir: Vec2,
    pub pass_vel: Option<Vec2>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub enum BallPossessionState {
    Possesed {
        owner: PlayerId,
        team: Team,
        capture_ticks: u32,
    }, 
    Passing { team: Team },
    Free
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum BallStagnationState {
    Active,
    Stagnant {
        center: Vec2,
        ticks: u32,
    },
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct BallState {
    pub pos: Vec2,
    pub vel: Vec2,
    pub radius: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct GameState {
    pub tick: u32,
    pub ball: BallState,
    pub ball_possession: BallPossessionState,
    pub ball_stagnation: BallStagnationState,
    pub players: [PlayerState; (NUM_PLAYERS * 2) as usize],
    // TODO goal owners, will they be used?
    pub score: TeamPair<u32>
}

impl GameState {
    #[inline(always)]
    fn is_ball_free(&self) -> bool {
        matches!(self.ball_possession, BallPossessionState::Free)
    }

    #[inline(always)]
    fn ball_owner(&self) -> Option<PlayerId> {
        if let BallPossessionState::Possesed { owner, .. } = self.ball_possession {
            Some(owner)
        } else {
            None
        }
    }

    #[inline(always)]
    fn player_team(&self, id: PlayerId) -> Option<Team> {
        if id < NUM_PLAYERS {
            Some(Team::A)
        } else if id < NUM_PLAYERS * 2 {
            Some(Team::B)
        } else {
            None
        }
    }

}

pub fn handle_player_collision(state: &mut GameState, conf: &GameConfig) {
    let mut rng = thread_rng();

    let mut iterations = 0;
    let mut resolved = true;
    let n = NUM_PLAYERS * 2;

    let mut pairs = Vec::new();
    pairs.reserve_exact((n * (n - 1) / 2) as usize);
    for i in 0..n {
        for j in (i+1)..n {
            pairs.push((i, j));
        }
    }

    while !resolved && iterations < COLLISION_MAX_ITERATIONS {
        resolved = true;
        pairs.shuffle(&mut rng);
        // player on player collision
        for (i, j) in pairs.iter().copied() {
            // i assert that i know what im doing
            // i am editing disjoint parts of the state object, and the borrow checker cannot
            // validate that
            let p1 = unsafe { &mut *state.players.as_mut_ptr().add(i as usize) };
            let p2 = unsafe { &mut *state.players.as_mut_ptr().add(j as usize) };
            let dist_sq = p1.pos.dist_sq(p2.pos);
            let min_dist = p1.radius + p2.radius;
            if dist_sq < min_dist.powi(2) {
                resolved = false;
                dist = dist_sq.sqrt();
            }
        }
        iterations += 1;
    }

}
