use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};

pub struct SessionOptions<'a> {
    pub voice: &'a str,
    pub instructions: &'a str,
    pub reasoning_effort: &'a str,
}

impl<'a> SessionOptions<'a> {
    pub fn to_session_update(&self) -> Value {
        let mut session = json!({
            "modalities": ["audio", "text"],
            "voice": self.voice,
            "instructions": self.instructions,
            "input_audio_format": "pcm16",
            "output_audio_format": "pcm16",
            "turn_detection": Value::Null,
        });
        if !self.reasoning_effort.is_empty() {
            session["reasoning"] = json!({"effort": self.reasoning_effort});
        }
        json!({"type": "session.update", "session": session})
    }
}

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn connect(
    url: &str,
    headers: &[(&'static str, String)],
    initial: Value,
) -> Result<WsStream, String> {
    let mut req = url
        .into_client_request()
        .map_err(|e| format!("realtime url: {e}"))?;
    for (name, value) in headers {
        req.headers_mut().insert(
            *name,
            value.parse().map_err(|e| format!("{name} header: {e}"))?,
        );
    }
    let (mut ws, _resp) = connect_async(req)
        .await
        .map_err(|e| format!("realtime connect: {e}"))?;
    let payload = serde_json::to_string(&initial).map_err(|e| format!("session.update encode: {e}"))?;
    ws.send(Message::Text(payload))
        .await
        .map_err(|e| format!("session.update send: {e}"))?;
    Ok(ws)
}

/// Fan-out a single WS connection into a writer-channel + reader-channel pair.
/// The relay sends events to `tx_send`; the relay reads upstream events from
/// `rx_recv`. Owning the WS in a dedicated task avoids the lock dance from
/// the Python side.
pub fn split_ws(
    ws: WsStream,
) -> (mpsc::UnboundedSender<Value>, mpsc::UnboundedReceiver<Value>) {
    let (tx_send, mut rx_send) = mpsc::unbounded_channel::<Value>();
    let (tx_recv, rx_recv) = mpsc::unbounded_channel::<Value>();

    let (mut sink, mut stream) = ws.split();

    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if tx_recv.send(v).is_err() {
                            break;
                        }
                    }
                }
                Ok(Message::Binary(_)) => {}
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => break,
                Ok(Message::Frame(_)) => {}
                Err(_) => break,
            }
        }
    });

    tokio::spawn(async move {
        while let Some(v) = rx_send.recv().await {
            let payload = match serde_json::to_string(&v) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if sink.send(Message::Text(payload)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    (tx_send, rx_recv)
}
