#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::similar_names)]
#![allow(clippy::single_match_else)]
#![allow(clippy::type_complexity)]

mod async_stdin;
mod client;
mod cmd;
mod component_future;
mod error;
mod key_reader;
mod process;
mod protocol;
mod server;
mod session_list;
mod term;
mod util;

fn main() {
    match crate::cmd::parse().and_then(|m| crate::cmd::run(&m)) {
        Ok(_) => {}
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }
}
