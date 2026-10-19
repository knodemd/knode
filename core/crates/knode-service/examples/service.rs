//! Runs Knode Service in the foreground until Ctrl-C or SIGTERM, as `knode service run` will.
//!
//! ```sh
//! KNODE_HOME=/tmp/knode-home cargo run -p knode-service --example service
//! ```

fn main() {
    let result = knode_service::Config::from_env().and_then(knode_service::run);
    if let Err(e) = result {
        eprintln!("knode service: {e}");
        std::process::exit(1);
    }
}
