use game_runner::*;
use std::fs::OpenOptions;
use memmap::MmapMut;
use std::sync::atomic::Ordering;

fn main () {
    println!("hello there");
    
    // Process 2 (signaler)  
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("mmap")
        .unwrap();

    let mmap = unsafe { MmapMut::map_mut(&file).unwrap() };
    let ptr = mmap.as_ptr() as *mut Shm;
    let shmem = unsafe { &mut *ptr };

    println!("i will signal now");

    loop {
        match shmem.sync.load(Ordering::Acquire) {
            ENGINE_READY => {
                match &mut shmem.shm {
                    ShmMsg::Init { config: _, response } => {
                        *response = 1.0;
                    },
                    ShmMsg::Tick { state, response } => {
                        *response = if state.ball_pos.1 > state.p0_pos {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                }
                shmem.sync.store(ENGINE_BUSY, Ordering::Release);
            },
            ENGINE_BUSY  => {
                continue;
            },
            ENGINE_FINISHED => {
                break;
            }
            _ => {
                panic!("unknown sync bit")
            }
        }

    }

    println!("signalled");
}
