use crate::prelude::*;

const LIST_URL: &str = "http://127.0.0.1:4145/list";
const WATCH_URL: &str = "ws://127.0.0.1:4145/watch";

struct WatchConn {
    ws: WebSocket,
    term: vt100::Parser,
}

impl Drop for WatchConn {
    fn drop(&mut self) {
        self.ws.close().unwrap();
    }
}

#[derive(Default)]
pub struct Model {
    sessions: Vec<crate::protocol::Session>,
    watch_conn: Option<WatchConn>,
}

impl Model {
    pub(crate) fn update(
        &mut self,
        msg: crate::Msg,
        orders: &mut impl Orders<crate::Msg>,
    ) {
        match msg {
            crate::Msg::List(sessions) => match sessions {
                Ok(sessions) => {
                    log::debug!("got sessions");
                    self.update_sessions(sessions);
                }
                Err(e) => {
                    log::error!("error getting sessions: {:?}", e);
                }
            },
            crate::Msg::Refresh => {
                log::debug!("refreshing");
                orders.perform_cmd(self.list());
            }
            crate::Msg::StartWatching(id) => {
                log::debug!("watching {}", id);
                self.watch(&id, orders);
            }
            crate::Msg::Watch(id, event) => match event {
                crate::ws::WebSocketEvent::Connected(_) => {
                    log::info!("{}: connected", id);
                }
                crate::ws::WebSocketEvent::Disconnected(_) => {
                    log::info!("{}: disconnected", id);
                }
                crate::ws::WebSocketEvent::Message(msg) => {
                    log::info!("{}: message: {:?}", id, msg);
                    let json = msg.data().as_string().unwrap();
                    let msg: crate::protocol::Message =
                        serde_json::from_str(&json).unwrap();
                    match msg {
                        crate::protocol::Message::TerminalOutput { data } => {
                            self.process(&data);
                        }
                        crate::protocol::Message::Disconnected => {
                            self.disconnect_watch();
                            orders.perform_cmd(self.list());
                        }
                        crate::protocol::Message::Resize { size } => {
                            self.set_size(size.rows, size.cols);
                        }
                    }
                }
                crate::ws::WebSocketEvent::Error(e) => {
                    log::error!("{}: error: {:?}", id, e);
                }
            },
            crate::Msg::StopWatching => {
                self.disconnect_watch();
                orders.perform_cmd(self.list());
            }
        }
    }

    pub(crate) fn list(
        &self,
    ) -> impl futures::Future<Item = crate::Msg, Error = crate::Msg> {
        seed::Request::new(LIST_URL).fetch_json_data(crate::Msg::List)
    }

    pub(crate) fn watch(
        &mut self,
        id: &str,
        orders: &mut impl Orders<crate::Msg>,
    ) {
        let ws = crate::ws::connect(
            &format!("{}?id={}", WATCH_URL, id),
            id,
            crate::Msg::Watch,
            orders,
        );
        let term = vt100::Parser::default();
        self.watch_conn = Some(WatchConn { ws, term })
    }

    pub fn sessions(&self) -> &[crate::protocol::Session] {
        &self.sessions
    }

    pub fn update_sessions(
        &mut self,
        sessions: Vec<crate::protocol::Session>,
    ) {
        self.sessions = sessions;
    }

    pub fn watching(&self) -> bool {
        self.watch_conn.is_some()
    }

    pub fn disconnect_watch(&mut self) {
        self.watch_conn = None;
    }

    pub fn process(&mut self, bytes: &[u8]) {
        if let Some(conn) = &mut self.watch_conn {
            conn.term.process(bytes);
        }
    }

    pub fn set_size(&mut self, rows: u16, cols: u16) {
        if let Some(conn) = &mut self.watch_conn {
            conn.term.set_size(rows, cols);
        }
    }

    pub fn screen(&self) -> Option<&vt100::Screen> {
        self.watch_conn.as_ref().map(|conn| conn.term.screen())
    }
}
