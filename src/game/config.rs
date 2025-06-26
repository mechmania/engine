use serde::{ Serialize, Deserialize };
use super::util::*;

pub const EPSILON: f32 = 0.001;
pub const COLLISION_MAX_ITERATIONS: u32 = 100;
pub const NUM_PLAYERS: u32 = 4;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct BallConfig {
    pub friction: f32,
    pub radius: f32,
    pub capture_ticks: u32,
    pub stagnation_radius: f32,
    pub stagnation_ticks: u32
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct PlayerConfig {
    pub radius: f32, 
    pub pickup_radius: f32,
    pub speed: f32,
    pub pass_speed: f32,
    pub pass_error: f32,
    pub possession_slowdown: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct FieldConfig {
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct GoalConfig {
    pub height: u32,
    pub thickness: u32,
    pub penalty_radius: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct HoardConfig {
    pub size: u32,
    pub radius: f32,
    pub debuf: f32,
}


#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[repr(C)]
pub struct GameConfig {
    pub max_ticks: u32,
    pub spawn_ball_dist: f32,
    //pub hoarding: HoardConfig,
    pub ball: BallConfig,
    pub player: PlayerConfig,
    pub field: FieldConfig,
    pub goal: GoalConfig,
}

impl FieldConfig {
    pub fn center(&self) -> Vec2 {
        Vec2::new(self.width as f32 * 0.5, self.height as f32 * 0.5)
    }

    pub fn bottom_right(&self) -> Vec2 {
        Vec2::new(self.width as f32, self.height as f32)
    }

    pub fn goal_a(&self) -> Vec2 {
        Vec2::new(0.0, self.height as f32 * 0.5)
    }

    pub fn goal_b(&self) -> Vec2 {
        Vec2::new(self.width as f32, self.height as f32 * 0.5)
    }
}
