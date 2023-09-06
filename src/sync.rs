extern crate redis;

use std::sync::{Arc, Mutex};
use std::thread;

use crate::backend::{RoundRobinBackend, GetBackend};

pub fn create_sync_thread(backend: Arc<Mutex<RoundRobinBackend>>, redis_url: String) {
    thread::spawn(move || {
        let pubsub = subscribe_to_redis(&redis_url).unwrap();
        loop {
            let msg = pubsub.get_message().unwrap();
            handle_message(backend.clone(), msg).unwrap();
        }
    });
}

fn subscribe_to_redis(url: &str) -> redis::RedisResult<redis::PubSub> {
    let client = redis::Client::open(url)?;
    let mut pubsub: redis::PubSub = client.get_pubsub()?;
    pubsub.subscribe("backend_add")?;
    pubsub.subscribe("backend_remove")?;
    info!("Subscribed to Redis channels 'backend_add' and 'backend_remove'");
    Ok(pubsub)
}

fn handle_message(backend: Arc<Mutex<RoundRobinBackend>>,
                  msg: redis::Msg)
                  -> redis::RedisResult<()> {
    let channel = msg.get_channel_name();
    let payload: String = msg.get_payload()?;
    debug!("New message on Redis channel {}: '{}'", channel, payload);

    match channel {
        "backend_add" => {
            let mut backend = backend.lock().unwrap();
            match backend.add(&payload) {
                Ok(_) => info!("Added new server {}", payload),
                _ => {}
            }
        }
        "backend_remove" => {
            let mut backend = backend.lock().unwrap();
            match backend.remove(&payload) {
                Ok(_) => info!("Removed server {}", payload),
                _ => {}
            }
        }
        _ => info!("Cannot parse Redis message"),
    }
    Ok(())
}
