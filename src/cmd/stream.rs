use crate::prelude::*;
use tokio::io::AsyncWrite as _;

pub fn cmd<'a, 'b>(app: clap::App<'a, 'b>) -> clap::App<'a, 'b> {
    app.about("Stream your terminal")
        .arg(
            clap::Arg::with_name("login-plain")
                .long("login-plain")
                .takes_value(true),
        )
        .arg(
            clap::Arg::with_name("login-recurse-center")
                .long("login-recurse-center")
                .conflicts_with("login-plain"),
        )
        .arg(
            clap::Arg::with_name("address")
                .long("address")
                .takes_value(true),
        )
        .arg(clap::Arg::with_name("tls").long("tls"))
        .arg(
            clap::Arg::with_name("buffer-size")
                .long("buffer-size")
                .takes_value(true),
        )
        .arg(clap::Arg::with_name("command").index(1))
        .arg(clap::Arg::with_name("args").index(2).multiple(true))
}

pub fn run<'a>(matches: &clap::ArgMatches<'a>) -> super::Result<()> {
    let auth = if matches.is_present("login-recurse-center") {
        crate::protocol::Auth::RecurseCenter { id: None }
    } else {
        let username = matches
            .value_of("login-plain")
            .map(std::string::ToString::to_string)
            .or_else(|| std::env::var("USER").ok())
            .context(crate::error::CouldntFindUsername)?;
        crate::protocol::Auth::Plain { username }
    };
    let (host, address) =
        crate::util::resolve_address(matches.value_of("address"))?;
    let tls = matches.is_present("tls");
    let buffer_size =
        matches
            .value_of("buffer-size")
            .map_or(Ok(4 * 1024 * 1024), |s| {
                s.parse()
                    .context(crate::error::ParseBufferSize { input: s })
            })?;
    let command = matches.value_of("command").map_or_else(
        || std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
        std::string::ToString::to_string,
    );
    let args = if let Some(args) = matches.values_of("args") {
        args.map(std::string::ToString::to_string).collect()
    } else {
        vec![]
    };
    run_impl(&auth, &host, address, tls, buffer_size, &command, &args)
}

fn run_impl(
    auth: &crate::protocol::Auth,
    host: &str,
    address: std::net::SocketAddr,
    tls: bool,
    buffer_size: usize,
    command: &str,
    args: &[String],
) -> Result<()> {
    let host = host.to_string();
    let fut: Box<
        dyn futures::future::Future<Item = (), Error = Error> + Send,
    > = if tls {
        let connector = native_tls::TlsConnector::new()
            .context(crate::error::CreateConnector)?;
        let connect: crate::client::Connector<_> = Box::new(move || {
            let host = host.clone();
            let connector = connector.clone();
            let connector = tokio_tls::TlsConnector::from(connector);
            let stream = tokio::net::tcp::TcpStream::connect(&address);
            Box::new(stream.context(crate::error::Connect).and_then(
                move |stream| {
                    connector
                        .connect(&host, stream)
                        .context(crate::error::ConnectTls)
                },
            ))
        });
        Box::new(StreamSession::new(
            command,
            args,
            connect,
            buffer_size,
            auth,
        ))
    } else {
        let connect: crate::client::Connector<_> = Box::new(move || {
            Box::new(
                tokio::net::tcp::TcpStream::connect(&address)
                    .context(crate::error::Connect),
            )
        });
        Box::new(StreamSession::new(
            command,
            args,
            connect,
            buffer_size,
            auth,
        ))
    };
    tokio::run(fut.map_err(|e| {
        eprintln!("{}", e);
    }));

    Ok(())
}

struct StreamSession<
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
> {
    client: crate::client::Client<S>,
    process: crate::process::Process<crate::async_stdin::Stdin>,
    stdout: tokio::io::Stdout,
    buffer: crate::term::Buffer,
    sent_local: usize,
    sent_remote: usize,
    needs_flush: bool,
    connected: bool,
    done: bool,
    raw_screen: Option<crossterm::RawScreen>,
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>
    StreamSession<S>
{
    fn new(
        cmd: &str,
        args: &[String],
        connect: crate::client::Connector<S>,
        buffer_size: usize,
        auth: &crate::protocol::Auth,
    ) -> Self {
        let client =
            crate::client::Client::stream(connect, auth, buffer_size);

        // TODO: tokio::io::stdin is broken (it's blocking)
        // see https://github.com/tokio-rs/tokio/issues/589
        // let input = tokio::io::stdin();
        let input = crate::async_stdin::Stdin::new();

        let process = crate::process::Process::new(cmd, args, input);

        Self {
            client,
            process,
            stdout: tokio::io::stdout(),
            buffer: crate::term::Buffer::new(buffer_size),
            sent_local: 0,
            sent_remote: 0,
            needs_flush: false,
            connected: false,
            done: false,
            raw_screen: None,
        }
    }

    fn record_bytes(&mut self, buf: &[u8]) {
        let truncated = self.buffer.append(buf);
        if truncated > self.sent_local {
            self.sent_local = 0;
        } else {
            self.sent_local -= truncated;
        }
        if truncated > self.sent_remote {
            self.sent_remote = 0;
        } else {
            self.sent_remote -= truncated;
        }
    }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>
    StreamSession<S>
{
    const POLL_FNS: &'static [&'static dyn for<'a> Fn(
        &'a mut Self,
    ) -> Result<
        crate::component_future::Poll<()>,
    >] = &[
        &Self::poll_read_client,
        &Self::poll_read_process,
        &Self::poll_write_terminal,
        &Self::poll_flush_terminal,
        &Self::poll_write_server,
    ];

    // this should never return Err, because we don't want server
    // communication issues to ever interrupt a running process
    fn poll_read_client(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        match self.client.poll() {
            Ok(futures::Async::Ready(Some(e))) => match e {
                crate::client::Event::Disconnect => {
                    self.connected = false;
                    Ok(crate::component_future::Poll::DidWork)
                }
                crate::client::Event::Start(size) => {
                    self.process.resize(size);
                    Ok(crate::component_future::Poll::DidWork)
                }
                crate::client::Event::Connect() => {
                    self.connected = true;
                    self.sent_remote = 0;
                    Ok(crate::component_future::Poll::DidWork)
                }
                crate::client::Event::ServerMessage(..) => {
                    // we don't expect to ever see a server message once we
                    // start streaming, so if one comes through, assume
                    // something is messed up and try again
                    self.client.reconnect();
                    Ok(crate::component_future::Poll::DidWork)
                }
                crate::client::Event::Resize(size) => {
                    self.process.resize(size);
                    Ok(crate::component_future::Poll::DidWork)
                }
            },
            Ok(futures::Async::Ready(None)) => {
                // the client should never exit on its own
                unreachable!()
            }
            Ok(futures::Async::NotReady) => {
                Ok(crate::component_future::Poll::NotReady)
            }
            Err(..) => {
                self.client.reconnect();
                Ok(crate::component_future::Poll::DidWork)
            }
        }
    }

    fn poll_read_process(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        match self.process.poll()? {
            futures::Async::Ready(Some(e)) => {
                match e {
                    crate::process::Event::CommandStart(..) => {
                        if self.raw_screen.is_none() {
                            self.raw_screen = Some(
                                crossterm::RawScreen::into_raw_mode()
                                    .context(crate::error::ToRawMode)?,
                            );
                        }
                        self.process.resize(crate::term::Size::get()?);
                    }
                    crate::process::Event::CommandExit(..) => {
                        self.done = true;
                    }
                    crate::process::Event::Output(output) => {
                        self.record_bytes(&output);
                    }
                }
                Ok(crate::component_future::Poll::DidWork)
            }
            futures::Async::Ready(None) => {
                if !self.done {
                    unreachable!()
                }
                // don't return final event here - wait until we are done
                // sending all data to the server (see poll_write_server)
                Ok(crate::component_future::Poll::DidWork)
            }
            futures::Async::NotReady => {
                Ok(crate::component_future::Poll::NotReady)
            }
        }
    }

    fn poll_write_terminal(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        if self.sent_local == self.buffer.len() {
            return Ok(crate::component_future::Poll::NothingToDo);
        }

        match self
            .stdout
            .poll_write(&self.buffer.contents()[self.sent_local..])
            .context(crate::error::WriteTerminal)?
        {
            futures::Async::Ready(n) => {
                self.sent_local += n;
                self.needs_flush = true;
                Ok(crate::component_future::Poll::DidWork)
            }
            futures::Async::NotReady => {
                Ok(crate::component_future::Poll::NotReady)
            }
        }
    }

    fn poll_flush_terminal(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        if !self.needs_flush {
            return Ok(crate::component_future::Poll::NothingToDo);
        }

        match self
            .stdout
            .poll_flush()
            .context(crate::error::FlushTerminal)?
        {
            futures::Async::Ready(()) => {
                self.needs_flush = false;
                Ok(crate::component_future::Poll::DidWork)
            }
            futures::Async::NotReady => {
                Ok(crate::component_future::Poll::NotReady)
            }
        }
    }

    fn poll_write_server(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        if self.sent_remote == self.buffer.len() || !self.connected {
            // ship all data to the server before actually ending
            if self.done {
                return Ok(crate::component_future::Poll::Event(()));
            } else {
                return Ok(crate::component_future::Poll::NothingToDo);
            }
        }

        let buf = &self.buffer.contents()[self.sent_remote..];
        self.client
            .send_message(crate::protocol::Message::terminal_output(buf));
        self.sent_remote = self.buffer.len();

        Ok(crate::component_future::Poll::DidWork)
    }
}

#[must_use = "futures do nothing unless polled"]
impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>
    futures::future::Future for StreamSession<S>
{
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> futures::Poll<Self::Item, Self::Error> {
        crate::component_future::poll_future(self, Self::POLL_FNS)
    }
}
