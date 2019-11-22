use crate::prelude::*;

const LIST_URL: &str = "http://127.0.0.1:4145/list";
const WATCH_URL: &str = "ws://127.0.0.1:4145/watch";

struct WatchConn {
    id: String,
    #[allow(dead_code)] // no idea why it thinks this is dead
    ws: WebSocket,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub username: String,
}

#[derive(Default)]
pub struct Model {
    sessions: Vec<Session>,
    watch_conn: Option<WatchConn>,
}

impl Model {
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
        let ws = crate::ws::connect(WATCH_URL, crate::Msg::Watch, orders);
        self.watch_conn = Some(WatchConn {
            id: id.to_string(),
            ws,
        })
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn update_sessions(&mut self, sessions: Vec<Session>) {
        self.sessions = sessions;
    }

    pub fn watch_id(&self) -> Option<&str> {
        if let Some(conn) = &self.watch_conn {
            Some(&conn.id)
        } else {
            None
        }
    }

    pub fn watch_disconnect(&mut self) {
        self.watch_conn = None;
    }
}