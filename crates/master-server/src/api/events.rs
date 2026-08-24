//! SSE 实时事件推送接口（第 16 节）。
//!
//! 前端建立 EventSource 连接 `/api/events`，实时获取节点、批次、任务、告警与代理的变更事件。
//! 强制要求通过鉴权（支持 Cookie 或 Bearer 鉴权）。

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::api::auth::AuthenticatedUser;
use crate::state::AppState;

/// GET /api/events
pub async fn sse_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg_res| match msg_res {
        Ok(event) => {
            let data_str = event.payload.to_string();
            Some(Ok(Event::default().event(event.kind).data(data_str)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive-ping"),
    )
}
