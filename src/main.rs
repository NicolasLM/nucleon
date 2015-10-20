#[macro_use] extern crate log;
extern crate argparse;
extern crate mio;

mod backend;
mod sync;
mod tcplb;
mod simplelogger;

use std::sync::{Arc, Mutex};
use std::process::exit;

use argparse::{ArgumentParser, StoreTrue, Store, Collect};
use mio::*;

fn main() {
    let mut servers: Vec<String> = Vec::new();
    let mut bind = "127.0.0.1:8000".to_string();
    let mut redis_url = "redis://localhost".to_string();
    let mut disable_redis = false;
    let mut log_level = "info".to_string();

    {
        let mut ap = ArgumentParser::new();
        ap.set_description("Dynamic TCP load balancer");

        ap.refer(&mut servers)
            .add_argument("server", Collect, "Servers to load balance");

        ap.refer(&mut bind)
            .add_option(&["-b", "--bind"], Store,
            "Bind the load balancer to address:port (127.0.0.1:8000)");

        ap.refer(&mut redis_url)
            .add_option(&["-r", "--redis"], Store,
            "URL of Redis database (redis://localhost)");

        ap.refer(&mut disable_redis)
            .add_option(&["--no-redis"], StoreTrue,
            "Disable updates of backend through Redis");
        
        ap.refer(&mut log_level)
            .add_option(&["-l", "--log"], Store,
            "Log level [debug, info, warn, error] (info)");

        ap.parse_args_or_exit();
    }

    simplelogger::init(&log_level).ok().expect("Failed to init logger");

    if servers.is_empty() {
        println!("Need at least one server to load balance");
        exit(1);
    }

    let backend = Arc::new(Mutex::new(
        backend::RoundRobinBackend::new(servers).unwrap()
    ));

    let mut proxy = tcplb::Proxy::new(&bind, backend.clone()); 
    let mut event_loop = EventLoop::new().unwrap();

    // Register interest in notifications of new connections
    event_loop.register_opt(&proxy.listen_sock, Token(1), EventSet::readable(),
                            PollOpt::edge()).unwrap();

    if !disable_redis {
        sync::create_sync_thread(backend.clone(), redis_url);
    }

    // Start handling events
    event_loop.run(&mut proxy).unwrap();

}
