use game_runner::{ 
    game::{ GameState, GameConfig, run_tick },
    ipc::{ BotChannel, ShmStage },
};

#[tokio::main]
async fn main() {
    let conf = GameConfig {
        height:         200,
        width:          400,
        paddle_length:  30,
        paddle_width:   5,
        ball_radius:    5,
        ball_speed:     5,
        winning_score:  5,
        max_ticks:      50000,
    };

    let on_init = |_| 0.0;
    let on_tick = |_, state: &GameState| {
        if state.ball_pos.1 > state.p0_pos {
            1.0
        } else {
            -1.0
        }
    };

    println!("{}", serde_json::to_string(&conf).expect("parse err"));

    let p1 = BotChannel::new();

    *p1.lock().await = ShmStage::Init {
        config: conf.clone(),
        action: 0.0
    };

    let mut stage = p1.lock().await;

    let ShmStage::Init { config: _, action } = *stage else {
        panic!();
    };

    let mut state = GameState {
        p0_pos:     on_init(&conf),
        p1_pos:     action,
        p0_score:   0,
        p1_score:   0,
        ball_pos:   (0.0, 0.0),
        ball_vel:   (-(conf.ball_speed as f64), conf.ball_speed as f64),
        tick:       0
    };

    *stage = ShmStage::Tick {
        state: state.clone(),
        action: 0.0
    };

    drop(stage);
     
    while state.tick < conf.max_ticks && state.p0_score < conf.winning_score && state.p1_score < conf.winning_score {

        let mut stage = p1.lock().await;

        let ShmStage::Tick { state: ref mut shm_state, action } = *stage else {
            panic!();
        };

        let p0 = on_tick(&conf, &state);
        run_tick(&mut state, &conf, p0, action);

        *shm_state = state.clone();

        println!("{}", serde_json::to_string(&state).expect("parse err"));
    }
}

