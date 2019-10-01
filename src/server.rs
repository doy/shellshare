use futures::stream::Stream as _;
use snafu::futures01::stream::StreamExt as _;
use snafu::futures01::FutureExt as _;
use tokio::io::AsyncRead as _;

#[derive(Debug, snafu::Snafu)]
pub enum Error {
    #[snafu(display(
        "failed to receive new socket over channel: {}",
        source
    ))]
    SocketChannelReceive {
        source: tokio::sync::mpsc::error::RecvError,
    },

    #[snafu(display(
        "failed to receive new socket over channel: channel closed"
    ))]
    SocketChannelClosed,

    #[snafu(display("failed to read message: {}", source))]
    ReadMessage { source: crate::protocol::Error },

    #[snafu(display("failed to write message: {}", source))]
    WriteMessage { source: crate::protocol::Error },

    #[snafu(display("unexpected message: {:?}", message))]
    UnexpectedMessage { message: crate::protocol::Message },

    #[snafu(display("unauthenticated message: {:?}", message))]
    UnauthenticatedMessage { message: crate::protocol::Message },

    #[snafu(display("invalid watch id: {}", id))]
    InvalidWatchId { id: String },
}

pub type Result<T> = std::result::Result<T, Error>;

enum ReadSocket {
    Connected(crate::protocol::FramedReader),
    Reading(
        Box<
            dyn futures::future::Future<
                    Item = (
                        crate::protocol::Message,
                        crate::protocol::FramedReader,
                    ),
                    Error = Error,
                > + Send,
        >,
    ),
}

enum WriteSocket {
    Connected(crate::protocol::FramedWriter),
    Writing(
        Box<
            dyn futures::future::Future<
                    Item = crate::protocol::FramedWriter,
                    Error = Error,
                > + Send,
        >,
    ),
}

struct Connection {
    rsock: Option<ReadSocket>,
    wsock: Option<WriteSocket>,

    ty: Option<crate::common::ConnectionType>,
    session: crate::common::Session,
    saved_data: crate::term::Buffer,

    to_send: std::collections::VecDeque<crate::protocol::Message>,
    closed: bool,
}

impl Connection {
    fn new(s: tokio::net::tcp::TcpStream) -> Self {
        let (rs, ws) = s.split();
        Self {
            rsock: Some(ReadSocket::Connected(
                crate::protocol::FramedReader::new(rs),
            )),
            wsock: Some(WriteSocket::Connected(
                crate::protocol::FramedWriter::new(ws),
            )),

            ty: None,
            session: crate::common::Session::new(),
            saved_data: crate::term::Buffer::new(),

            to_send: std::collections::VecDeque::new(),
            closed: false,
        }
    }

    fn close(&mut self, res: Result<()>) {
        let msg = match res {
            Ok(()) => crate::protocol::Message::disconnected(),
            Err(e) => crate::protocol::Message::error(&format!("{}", e)),
        };
        self.to_send.push_back(msg);
        self.closed = true;
    }
}

pub struct Server {
    sock_stream: Box<
        dyn futures::stream::Stream<Item = Connection, Error = Error> + Send,
    >,
    connections: std::collections::HashMap<String, Connection>,
}

impl Server {
    pub fn new(
        sock_r: tokio::sync::mpsc::Receiver<tokio::net::tcp::TcpStream>,
    ) -> Self {
        let sock_stream =
            sock_r.map(Connection::new).context(SocketChannelReceive);
        Self {
            sock_stream: Box::new(sock_stream),
            connections: std::collections::HashMap::new(),
        }
    }

    fn handle_message(
        &mut self,
        conn: &mut Connection,
        message: crate::protocol::Message,
    ) -> Result<()> {
        if conn.session.metadata.is_none() {
            self.handle_login_message(conn, message)
        } else {
            match conn.ty {
                Some(crate::common::ConnectionType::Casting) => {
                    self.handle_cast_message(conn, message)
                }
                Some(crate::common::ConnectionType::Watching(..)) => {
                    self.handle_watch_message(conn, message)
                }
                None => self.handle_other_message(conn, message),
            }
        }
    }

    fn handle_login_message(
        &mut self,
        conn: &mut Connection,
        message: crate::protocol::Message,
    ) -> Result<()> {
        match message {
            crate::protocol::Message::Login {
                username,
                term_type,
                ..
            } => {
                println!("got a connection from {}", username);
                conn.session.connect(&username, &term_type);
                Ok(())
            }
            m => Err(Error::UnauthenticatedMessage { message: m }),
        }
    }

    fn handle_cast_message(
        &mut self,
        conn: &mut Connection,
        message: crate::protocol::Message,
    ) -> Result<()> {
        let session = &conn.session;
        // we test for metadata being Some before calling handle_cast_message
        let metadata = session.metadata.as_ref().unwrap();
        match message {
            crate::protocol::Message::Heartbeat => {
                println!("got a heartbeat from {}", metadata.username);
                conn.to_send
                    .push_back(crate::protocol::Message::heartbeat());
                Ok(())
            }
            crate::protocol::Message::TerminalOutput { data } => {
                println!("got {} bytes of cast data", data.len());
                conn.saved_data.append(&data);
                for watch_conn in self.watchers_mut() {
                    if let Some(crate::common::ConnectionType::Watching(id)) =
                        &watch_conn.ty
                    {
                        if &session.id == id {
                            watch_conn.to_send.push_back(
                                crate::protocol::Message::terminal_output(
                                    &data,
                                ),
                            );
                        }
                    } else {
                        unreachable!()
                    }
                }
                Ok(())
            }
            m => Err(Error::UnexpectedMessage { message: m }),
        }
    }

    fn handle_watch_message(
        &mut self,
        conn: &mut Connection,
        message: crate::protocol::Message,
    ) -> Result<()> {
        let session = &conn.session;
        // we test for session being Some before calling handle_watch_message
        let metadata = session.metadata.as_ref().unwrap();
        match message {
            crate::protocol::Message::Heartbeat => {
                println!("got a heartbeat from {}", metadata.username);
                conn.to_send
                    .push_back(crate::protocol::Message::heartbeat());
                Ok(())
            }
            m => Err(Error::UnexpectedMessage { message: m }),
        }
    }

    fn handle_other_message(
        &mut self,
        conn: &mut Connection,
        message: crate::protocol::Message,
    ) -> Result<()> {
        match message {
            crate::protocol::Message::ListSessions => {
                let sessions: Vec<_> = self
                    .casters()
                    .map(|conn| &conn.session)
                    .filter(|session| session.metadata.is_some())
                    .cloned()
                    .collect();
                conn.to_send
                    .push_back(crate::protocol::Message::sessions(&sessions));
                Ok(())
            }
            crate::protocol::Message::StartCasting => {
                conn.ty = Some(crate::common::ConnectionType::Casting);
                Ok(())
            }
            crate::protocol::Message::StartWatching { id } => {
                if let Some(cast_conn) = self.connections.get(&id) {
                    let data = cast_conn.saved_data.contents().to_vec();
                    conn.ty =
                        Some(crate::common::ConnectionType::Watching(id));
                    conn.to_send.push_back(
                        crate::protocol::Message::terminal_output(&data),
                    );
                    Ok(())
                } else {
                    Err(Error::InvalidWatchId { id })
                }
            }
            m => Err(Error::UnexpectedMessage { message: m }),
        }
    }

    fn handle_disconnect(&mut self, conn: &mut Connection) {
        println!("disconnect");

        for watch_conn in self.watchers_mut() {
            if let Some(crate::common::ConnectionType::Watching(id)) =
                &watch_conn.ty
            {
                if id == &conn.session.id {
                    watch_conn.close(Ok(()));
                }
            } else {
                unreachable!()
            }
        }
    }

    fn poll_read_connection(
        &mut self,
        conn: &mut Connection,
    ) -> Result<crate::component_future::Poll<()>> {
        match &mut conn.rsock {
            Some(ReadSocket::Connected(..)) => {
                if let Some(ReadSocket::Connected(s)) = conn.rsock.take() {
                    let fut = Box::new(
                        crate::protocol::Message::read_async(s)
                            .context(ReadMessage),
                    );
                    conn.rsock = Some(ReadSocket::Reading(fut));
                } else {
                    unreachable!()
                }
                Ok(crate::component_future::Poll::DidWork)
            }
            Some(ReadSocket::Reading(fut)) => {
                match fut.poll() {
                    Ok(futures::Async::Ready((msg, s))) => {
                        let res = self.handle_message(conn, msg);
                        if res.is_err() {
                            conn.close(res);
                        }
                        conn.rsock = Some(ReadSocket::Connected(s));
                        Ok(crate::component_future::Poll::DidWork)
                    }
                    Ok(futures::Async::NotReady) => {
                        Ok(crate::component_future::Poll::NotReady)
                    }
                    Err(e) => {
                        if let Error::ReadMessage { ref source } = e {
                            match source {
                                crate::protocol::Error::ReadAsync {
                                    source: ref tokio_err,
                                } => {
                                    if tokio_err.kind()
                                        == tokio::io::ErrorKind::UnexpectedEof
                                    {
                                        Ok(crate::component_future::Poll::Event(()))
                                    } else {
                                        Err(e)
                                    }
                                }
                                crate::protocol::Error::EOF => Ok(
                                    crate::component_future::Poll::Event(()),
                                ),
                                _ => Err(e),
                            }
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            _ => Ok(crate::component_future::Poll::NothingToDo),
        }
    }

    fn poll_write_connection(
        &mut self,
        conn: &mut Connection,
    ) -> Result<crate::component_future::Poll<()>> {
        match &mut conn.wsock {
            Some(WriteSocket::Connected(..)) => {
                if let Some(msg) = conn.to_send.pop_front() {
                    if let Some(WriteSocket::Connected(s)) = conn.wsock.take()
                    {
                        let fut = msg.write_async(s).context(WriteMessage);
                        conn.wsock =
                            Some(WriteSocket::Writing(Box::new(fut)));
                    } else {
                        unreachable!()
                    }
                    Ok(crate::component_future::Poll::DidWork)
                } else if conn.closed {
                    Ok(crate::component_future::Poll::Event(()))
                } else {
                    Ok(crate::component_future::Poll::NothingToDo)
                }
            }
            Some(WriteSocket::Writing(fut)) => {
                match fut.poll() {
                    Ok(futures::Async::Ready(s)) => {
                        conn.wsock = Some(WriteSocket::Connected(s));
                        Ok(crate::component_future::Poll::DidWork)
                    }
                    Ok(futures::Async::NotReady) => {
                        Ok(crate::component_future::Poll::NotReady)
                    }
                    Err(e) => {
                        if let Error::WriteMessage { ref source } = e {
                            match source {
                                crate::protocol::Error::WriteAsync {
                                    source: ref tokio_err,
                                } => {
                                    if tokio_err.kind()
                                        == tokio::io::ErrorKind::UnexpectedEof
                                    {
                                        Ok(crate::component_future::Poll::Event(()))
                                    } else {
                                        Err(e)
                                    }
                                }
                                crate::protocol::Error::EOF => Ok(
                                    crate::component_future::Poll::Event(()),
                                ),
                                _ => Err(e),
                            }
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            _ => Ok(crate::component_future::Poll::NothingToDo),
        }
    }

    fn casters(&self) -> impl Iterator<Item = &Connection> {
        self.connections.values().filter(|conn| {
            if conn.session.metadata.is_none() {
                return false;
            }

            conn.ty == Some(crate::common::ConnectionType::Casting)
        })
    }

    fn watchers_mut(&mut self) -> impl Iterator<Item = &mut Connection> {
        self.connections.values_mut().filter(|conn| {
            if conn.session.metadata.is_none() {
                return false;
            }

            if let Some(crate::common::ConnectionType::Watching(..)) = conn.ty
            {
                true
            } else {
                false
            }
        })
    }
}

impl Server {
    const POLL_FNS: &'static [&'static dyn for<'a> Fn(
        &'a mut Self,
    ) -> Result<
        crate::component_future::Poll<()>,
    >] = &[
        &Self::poll_new_connections,
        &Self::poll_read,
        &Self::poll_write,
    ];

    fn poll_new_connections(
        &mut self,
    ) -> Result<crate::component_future::Poll<()>> {
        match self.sock_stream.poll() {
            Ok(futures::Async::Ready(Some(conn))) => {
                self.connections.insert(conn.session.id.clone(), conn);
                Ok(crate::component_future::Poll::DidWork)
            }
            Ok(futures::Async::Ready(None)) => {
                Err(Error::SocketChannelClosed)
            }
            Ok(futures::Async::NotReady) => {
                Ok(crate::component_future::Poll::NotReady)
            }
            Err(e) => Err(e),
        }
    }

    fn poll_read(&mut self) -> Result<crate::component_future::Poll<()>> {
        let mut did_work = false;
        let mut not_ready = false;

        let keys: Vec<_> = self.connections.keys().cloned().collect();
        for key in keys {
            let mut conn = self.connections.remove(&key).unwrap();
            match self.poll_read_connection(&mut conn) {
                Ok(crate::component_future::Poll::Event(())) => {
                    self.handle_disconnect(&mut conn);
                    continue;
                }
                Ok(crate::component_future::Poll::DidWork) => {
                    did_work = true;
                }
                Ok(crate::component_future::Poll::NotReady) => {
                    not_ready = true;
                }
                Err(e) => {
                    println!("error reading from active connection: {}", e);
                    continue;
                }
                _ => {}
            }
            self.connections.insert(key.to_string(), conn);
        }

        if did_work {
            Ok(crate::component_future::Poll::DidWork)
        } else if not_ready {
            Ok(crate::component_future::Poll::NotReady)
        } else {
            Ok(crate::component_future::Poll::NothingToDo)
        }
    }

    fn poll_write(&mut self) -> Result<crate::component_future::Poll<()>> {
        let mut did_work = false;
        let mut not_ready = false;

        let keys: Vec<_> = self.connections.keys().cloned().collect();
        for key in keys {
            let mut conn = self.connections.remove(&key).unwrap();
            match self.poll_write_connection(&mut conn) {
                Ok(crate::component_future::Poll::Event(())) => {
                    self.handle_disconnect(&mut conn);
                    continue;
                }
                Ok(crate::component_future::Poll::DidWork) => {
                    did_work = true;
                }
                Ok(crate::component_future::Poll::NotReady) => {
                    not_ready = true;
                }
                Err(e) => {
                    println!("error reading from active connection: {}", e);
                    continue;
                }
                _ => {}
            }
            self.connections.insert(key.to_string(), conn);
        }

        if did_work {
            Ok(crate::component_future::Poll::DidWork)
        } else if not_ready {
            Ok(crate::component_future::Poll::NotReady)
        } else {
            Ok(crate::component_future::Poll::NothingToDo)
        }
    }
}

#[must_use = "futures do nothing unless polled"]
impl futures::future::Future for Server {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> futures::Poll<Self::Item, Self::Error> {
        crate::component_future::poll_future(self, Self::POLL_FNS)
    }
}
