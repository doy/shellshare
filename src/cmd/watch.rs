use snafu::ResultExt as _;

#[derive(Debug, snafu::Snafu)]
pub enum Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn cmd<'a, 'b>(app: clap::App<'a, 'b>) -> clap::App<'a, 'b> {
    app.about("Watch termcast streams")
}

pub fn run<'a>(_matches: &clap::ArgMatches<'a>) -> super::Result<()> {
    run_impl().context(super::Watch)
}

fn run_impl() -> Result<()> {
    unimplemented!()
}
