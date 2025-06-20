use serde::{ Serialize, Deserialize };

pub const EPSILON: f32 = 0.001;
pub const COLLISION_MAX_ITERATIONS: u32 = 100;
pub const NUM_PLAYERS: u32 = 4;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct BallConfig {
    pub friction: f32,
    pub radius: f32,
    pub capture_ticks: u32,
    pub stagnation_radius: f32,
    pub stagnation_ticks: u32
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct PlayerConfig {
    pub radius: f32, 
    pub pickup_radius: f32,
    pub speed: f32,
    pub pass_speed: f32,
    pub pass_error: f32,
    pub posession_slowdown: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct FieldConfig {
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct GoalConfig {
    pub height: u32,
    pub penalty_radius: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct HoardConfig {
    pub size: u32,
    pub radius: f32,
    pub debuf: f32,
}


#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct GameConfig {
    pub max_ticks: u32,
    pub hoarding: HoardConfig,
    pub ball: BallConfig,
    pub player: PlayerConfig,
    pub field: FieldConfig,
    pub goal: GoalConfig,
}
