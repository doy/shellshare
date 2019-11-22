use crate::prelude::*;

pub(crate) fn render(model: &crate::model::Model) -> Vec<Node<crate::Msg>> {
    let rows: Vec<_> = model.sessions().iter().map(row).collect();
    vec![
        seed::table![
            seed::tr![
                seed::th!["username"],
                seed::th!["size"],
                seed::th!["idle"],
                seed::th!["watchers"],
                seed::th!["title"],
            ],
            rows
        ],
        seed::button![simple_ev(Ev::Click, crate::Msg::Refresh), "refresh"],
    ]
}

fn row(session: &crate::protocol::Session) -> Node<crate::Msg> {
    seed::tr![
        seed::td![seed::a![
            simple_ev(
                Ev::Click,
                crate::Msg::StartWatching(session.id.clone())
            ),
            session.username,
        ]],
        seed::td![format!("{}x{}", session.size.cols, session.size.rows)],
        seed::td![format_time(session.idle_time)],
        seed::td![format!("{}", session.watchers)],
        seed::td![session.title],
    ]
}

// XXX copied from teleterm
fn format_time(dur: u32) -> String {
    let secs = dur % 60;
    let dur = dur / 60;
    if dur == 0 {
        return format!("{}s", secs);
    }

    let mins = dur % 60;
    let dur = dur / 60;
    if dur == 0 {
        return format!("{}m{:02}s", mins, secs);
    }

    let hours = dur % 24;
    let dur = dur / 24;
    if dur == 0 {
        return format!("{}h{:02}m{:02}s", hours, mins, secs);
    }

    let days = dur;
    format!("{}d{:02}h{:02}m{:02}s", days, hours, mins, secs)
}
