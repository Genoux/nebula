//! The seam WebRTC will slot into.
//!
//! Everything above this point deals in JSON strings and PCM samples, so a
//! second implementation has to provide a duplex of text events and nothing
//! else. That is deliberately the narrowest useful interface: a WebRTC
//! transport carries audio on a media track rather than inside these events,
//! and [`Transport::send_audio`] is separate from [`Transport::send`] for
//! exactly that reason - it is the one call whose implementation would differ.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::protocol;

pub trait Transport: Send {
    fn send(&self, json: String) -> Result<()>;
    fn send_audio(&self, pcm: &[i16]) -> Result<()>;
    fn events(&mut self) -> &mut mpsc::Receiver<String>;
    fn close(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

pub struct WebSocketTransport {
    outgoing: mpsc::Sender<Message>,
    incoming: mpsc::Receiver<String>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl WebSocketTransport {
    pub async fn connect(api_key: &str) -> Result<Self> {
        let mut request = protocol::URL
            .into_client_request()
            .map_err(|_| anyhow!("invalid GPT-Live URL"))?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| anyhow!("invalid API key format"))?;
        authorization.set_sensitive(true);
        request.headers_mut().insert("Authorization", authorization);
        let (socket, _) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(|_| anyhow!("GPT-Live connection timed out"))?
        .map_err(|error| match error {
            tokio_tungstenite::tungstenite::Error::Http(response)
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                anyhow!(AuthenticationError)
            }
            _ => anyhow!(
                "could not open the GPT-Live connection; check network, model access and quota"
            ),
        })?;
        Ok(Self::spawn(socket))
    }

    fn spawn<S>(mut socket: tokio_tungstenite::WebSocketStream<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, mut outgoing_rx) = mpsc::channel::<Message>(64);
        let (incoming_tx, incoming) = mpsc::channel::<String>(128);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    _ = incoming_tx.closed() => break,
                    message = socket.next() => match message {
                        Some(Ok(Message::Text(text))) => {
                            if incoming_tx.try_send(text.to_string()).is_err() { break; }
                        }
                        Some(Ok(Message::Ping(_))) => {
                            if socket.flush().await.is_err() { break; }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        _ => {}
                    },
                    message = outgoing_rx.recv() => match message {
                        Some(message) => {
                            if tokio::time::timeout(std::time::Duration::from_secs(2), socket.send(message))
                                .await.map_or(true, |result| result.is_err()) { break; }
                        }
                        None => break,
                    }
                }
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), socket.close(None))
                .await;
        });
        Self {
            outgoing,
            incoming,
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

#[derive(Debug)]
pub struct AuthenticationError;

impl std::fmt::Display for AuthenticationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GPT-Live authentication failed; check the API key and project access")
    }
}
impl std::error::Error for AuthenticationError {}

impl Drop for WebSocketTransport {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

impl Transport for WebSocketTransport {
    fn send(&self, json: String) -> Result<()> {
        self.outgoing
            .try_send(Message::Text(json.into()))
            .map_err(|_| anyhow!("GPT-Live connection closed or send queue full"))
    }

    fn send_audio(&self, pcm: &[i16]) -> Result<()> {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for sample in pcm {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        self.send(protocol::append_audio(&encoded))
    }

    fn events(&mut self) -> &mut mpsc::Receiver<String> {
        &mut self.incoming
    }

    fn close(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(mut worker) = self.worker.take()
                && tokio::time::timeout(std::time::Duration::from_secs(1), &mut worker)
                    .await
                    .is_err()
            {
                worker.abort();
                let _ = worker.await;
            }
        })
    }
}

/// PCM16 little-endian, which is what the API documents and what
/// [`Transport::send_audio`] produces.
pub fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

pub fn from_pcm16_base64(encoded: &str) -> Result<Vec<f32>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("assistant audio was not valid base64")?;
    if !bytes.len().is_multiple_of(2) {
        anyhow::bail!("assistant audio contains an incomplete PCM sample");
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / i16::MAX as f32)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_client_has_a_crypto_provider() {
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let name = "api.openai.com".try_into().unwrap();
        assert!(rustls::ClientConnection::new(std::sync::Arc::new(config), name).is_ok());
    }

    async fn pair() -> (
        WebSocketTransport,
        tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(8192);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let server = tokio_tungstenite::WebSocketStream::from_raw_socket(
            server,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        (WebSocketTransport::spawn(client), server)
    }

    #[tokio::test]
    async fn closing_sends_a_close_frame_and_joins_the_worker() {
        let (mut transport, mut server) = pair().await;
        transport.send("test".into()).unwrap();
        assert!(matches!(server.next().await, Some(Ok(Message::Text(_)))));
        transport.close().await;
        assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
        assert!(transport.worker.is_none());
        assert!(transport.send("after close".into()).is_err());
    }

    #[tokio::test]
    async fn dropping_a_session_closes_its_socket() {
        let (transport, mut server) = pair().await;
        drop(transport);
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), server.next()).await;
        assert!(matches!(result, Ok(None | Some(Err(_)))));
    }

    #[tokio::test]
    async fn a_full_send_queue_fails_without_echoing_payload() {
        let (transport, _server) = pair().await;
        for _ in 0..64 {
            transport.send("private audio".into()).unwrap();
        }
        let error = transport.send("private audio".into()).unwrap_err();
        assert!(!format!("{error:#}").contains("private audio"));
    }

    #[tokio::test]
    async fn ping_is_answered_while_the_microphone_is_idle() {
        let (transport, mut server) = pair().await;
        server.send(Message::Ping(vec![1, 2].into())).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), server.next())
            .await
            .unwrap();
        assert!(matches!(result, Some(Ok(Message::Pong(_)))));
        drop(transport);
    }

    #[test]
    fn pcm16_round_trips_through_base64() {
        let original = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let pcm = to_pcm16(&original);
        let mut bytes = Vec::new();
        for s in &pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let back = from_pcm16_base64(&encoded).unwrap();
        for (a, b) in original.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-4, "{a} became {b}");
        }
    }

    #[test]
    fn clipping_does_not_wrap_around() {
        // Without the clamp, 2.0 * 32767 overflows i16 and a loud sample comes
        // out as a loud sample of the opposite sign - which is heard as a click.
        let pcm = to_pcm16(&[2.0, -2.0]);
        assert_eq!(pcm, vec![i16::MAX, -i16::MAX]);
    }

    #[test]
    fn an_odd_byte_count_does_not_panic() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        assert!(from_pcm16_base64(&encoded).is_err());
    }

    #[test]
    fn bad_base64_is_an_error_not_a_panic() {
        assert!(from_pcm16_base64("not base64 !!!").is_err());
    }
}
