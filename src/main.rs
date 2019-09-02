mod cmd;
mod protocol;
mod util;

fn main() {
    match crate::cmd::parse().and_then(crate::cmd::run) {
        Ok(_) => {}
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }
}
