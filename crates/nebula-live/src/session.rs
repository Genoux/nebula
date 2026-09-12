use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::protocol::{self, ServerEvent};
use crate::transport::{self, Transport};

pub const INSTRUCTIONS: &str = "You are Nebula, a concise, helpful Linux and coding voice assistant. \
Speak warmly and naturally. Default to short answers.\n\
Backchannel policy: Use moderate backchannels without competing with the user.\n\
Interruption policy: Stop speaking when the user interrupts and listen. \
Ignore your own speaker echo, music and unrelated room sounds.\n\
Delegation policy:\nBackend tools: Reasoning about coding, Linux and general questions; no tools or computer access.\n\
Delegate to the backend when a request needs careful reasoning or a correction changes the requested work.\n\
Do not delegate for greetings, brief clarifications, or answers already in the conversation.\n\
Delegate before answering questions that need backend reasoning. Do not guess the result while waiting.";

pub const BACKEND_INSTRUCTIONS: &str = "Help Nebula answer the user's spoken questions accurately. \
Transcripts may contain mistakes or corrections; use the latest context. Ask for clarification \
when necessary. Return concise facts and explanations suitable for speech. You have no tools, \
web access or access to the user's computer. Never claim to execute commands, inspect files or \
complete external actions.";

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Audio(Vec<f32>),
    UserActivity,
    StopRequested,
    BackendActivity,
    Closed,
    Failed(String),
}

#[derive(Debug, Default, Clone)]
pub struct Metrics {
    pub connect: Option<Duration>,
    pub first_audio_to_playback: Option<Duration>,
    pub voice_seconds: Option<f64>,
    pub finalized: bool,
}

impl Metrics {
    pub fn report(&self) -> String {
        fn ms(d: Option<Duration>) -> String {
            d.map_or_else(
                || "-".into(),
                |d| format!("{:.0}ms", d.as_secs_f64() * 1000.0),
            )
        }
        format!(
            "connect {} | first audio -> played {} | voice {}s | finalized {}",
            ms(self.connect),
            ms(self.first_audio_to_playback),
            self.voice_seconds
                .map_or_else(|| "-".into(), |s| format!("{s:.1}")),
            self.finalized
        )
    }
}

pub struct Session<T: Transport> {
    transport: T,
    transcript: String,
    transcript_end_ms: Option<u64>,
    pending_mute: Option<(bool, String, Instant)>,
    muted: bool,
    mute_sequence: u64,
    backend_responses: HashSet<String>,
    pub metrics: Metrics,
}

impl<T: Transport> Session<T> {
    pub async fn start(
        mut transport: T,
        start: protocol::SessionStart,
        started: Instant,
    ) -> Result<(Self, mpsc::Receiver<String>)> {
        transport
            .send(serde_json::to_string(&start).context("could not serialize session.start")?)?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let raw = transport
                    .events()
                    .recv()
                    .await
                    .context("GPT-Live closed before session.started")?;
                match serde_json::from_str::<ServerEvent>(&raw)
                    .context("invalid GPT-Live startup event")?
                {
                    ServerEvent::Started { session } => {
                        if session.model != start.session.model {
                            bail!("GPT-Live acknowledged an unexpected model");
                        }
                        return Ok::<(), anyhow::Error>(());
                    }
                    ServerEvent::Error { error } => bail!(safe_error(&error)),
                    ServerEvent::Closed { .. } => bail!("GPT-Live closed during startup"),
                    _ => {}
                }
            }
        })
        .await
        .context("GPT-Live session startup timed out")??;
        let events = std::mem::replace(transport.events(), mpsc::channel(128).1);
        Ok((
            Self {
                transport,
                transcript: String::new(),
                transcript_end_ms: None,
                pending_mute: None,
                muted: false,
                mute_sequence: 0,
                backend_responses: HashSet::new(),
                metrics: Metrics {
                    connect: Some(started.elapsed()),
                    ..Default::default()
                },
            },
            events,
        ))
    }

    pub fn request_reasoning(&self, text: &str) -> Result<()> {
        self.transport.send(serde_json::json!({
            "type":"response.item.create", "event_id":"nebula_probe_input",
            "item":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}
        }).to_string())?;
        self.transport.send(
            serde_json::json!({"type":"response.create","event_id":"nebula_probe_response"})
                .to_string(),
        )
    }

    pub fn append_instructions(&self, content: &str) -> Result<()> {
        self.transport.send(
            serde_json::json!({
                "type":"session.instructions.append", "event_id":"nebula_instruction",
                "delegation_id":null, "content":content
            })
            .to_string(),
        )
    }

    pub fn send_microphone(&self, samples: &[f32], muted: bool) -> Result<()> {
        if self.metrics.finalized {
            bail!("GPT-Live session has closed");
        }
        let pcm = if muted {
            vec![0; samples.len()]
        } else {
            transport::to_pcm16(samples)
        };
        self.transport.send_audio(&pcm)
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        if self.muted != muted {
            self.mute_sequence += 1;
            let id = format!("nebula_mute_{}", self.mute_sequence);
            self.transport.send(protocol::mute(muted, &id))?;
            self.pending_mute = Some((muted, id, Instant::now()));
            self.muted = muted;
        }
        if self
            .pending_mute
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() > Duration::from_secs(5))
        {
            bail!("GPT-Live did not acknowledge microphone control");
        }
        Ok(())
    }

    pub fn backend_busy(&self) -> bool {
        !self.backend_responses.is_empty()
    }

    pub fn handle(&mut self, raw: &str) -> Option<SessionEvent> {
        let event = match serde_json::from_str::<ServerEvent>(raw) {
            Ok(event) => event,
            Err(_) => {
                return Some(SessionEvent::Failed(
                    "Invalid GPT-Live event received".into(),
                ));
            }
        };
        match event {
            ServerEvent::Audio { delta } => match transport::from_pcm16_base64(&delta) {
                Ok(pcm) => Some(SessionEvent::Audio(pcm)),
                Err(_) => Some(SessionEvent::Failed(
                    "Invalid assistant audio received".into(),
                )),
            },
            ServerEvent::InputTranscript {
                delta,
                start_ms,
                end_ms,
            } => {
                if self.muted {
                    return None;
                }
                if self
                    .transcript_end_ms
                    .is_some_and(|last| start_ms > last.saturating_add(2000))
                {
                    self.transcript.clear();
                }
                self.transcript_end_ms = Some(end_ms);
                self.transcript.push_str(&delta);
                // Only the stop phrase needs local text; cap the rolling fragment window.
                if self.transcript.len() > 512 {
                    let mut at = self.transcript.len() - 512;
                    while !self.transcript.is_char_boundary(at) {
                        at += 1;
                    }
                    self.transcript.drain(..at);
                }
                if is_stop_command(&self.transcript) {
                    Some(SessionEvent::StopRequested)
                } else {
                    Some(SessionEvent::UserActivity)
                }
            }
            ServerEvent::Response { event } => {
                match event.kind.as_str() {
                    "response.created" => {
                        if let Some(response) = event.response {
                            self.backend_responses.insert(response.id);
                        }
                    }
                    "response.completed" | "response.cancelled" | "response.incomplete" => {
                        if let Some(response) = event.response {
                            self.backend_responses.remove(&response.id);
                        }
                    }
                    "response.failed" | "error" => {
                        return Some(SessionEvent::Failed(
                            "GPT-Live reasoning backend failed".into(),
                        ));
                    }
                    _ => return None,
                }
                Some(SessionEvent::BackendActivity)
            }
            ServerEvent::Muted { client_event_id } => {
                self.ack_mute(true, &client_event_id);
                None
            }
            ServerEvent::Unmuted { client_event_id } => {
                self.ack_mute(false, &client_event_id);
                None
            }
            ServerEvent::Usage { usage } => {
                self.metrics.voice_seconds = Some(usage.seconds);
                None
            }
            ServerEvent::Closed { usage, reason } => {
                self.metrics.voice_seconds = Some(usage.seconds);
                self.metrics.finalized = true;
                self.backend_responses.clear();
                if matches!(
                    reason.as_str(),
                    "close_requested" | "expired" | "remote_hangup"
                ) {
                    Some(SessionEvent::Closed)
                } else {
                    Some(SessionEvent::Failed(
                        "GPT-Live session ended unexpectedly".into(),
                    ))
                }
            }
            ServerEvent::Error { error } => Some(SessionEvent::Failed(safe_error(&error).into())),
            _ => None,
        }
    }

    fn ack_mute(&mut self, muted: bool, id: &str) {
        if self
            .pending_mute
            .as_ref()
            .is_some_and(|(wanted, pending, _)| *wanted == muted && pending == id)
        {
            self.pending_mute = None;
        }
    }

    pub async fn close(&mut self, events: &mut mpsc::Receiver<String>) {
        if !self.metrics.finalized && self.transport.send(protocol::close()).is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(15), async {
                while let Some(raw) = events.recv().await {
                    self.handle(&raw);
                    if self.metrics.finalized {
                        break;
                    }
                }
            })
            .await;
        }
        if !self.metrics.finalized {
            tracing::warn!("GPT-Live finalization incomplete; final usage is unconfirmed");
        }
        self.transport.close().await;
    }
}

pub fn is_stop_command(transcript: &str) -> bool {
    let lowered = transcript.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().any(|(i, &word)| {
        word == "nebula"
            && words[i + 1..]
                .iter()
                .take(3)
                .take_while(|w| !matches!(**w, "don" | "t" | "not" | "never" | "dont"))
                .any(|w| *w == "stop")
    })
}

fn safe_error(error: &protocol::ApiError) -> &'static str {
    match error.code.as_deref() {
        Some("invalid_api_key" | "authentication_error") => "GPT-Live authentication failed",
        Some("insufficient_quota" | "rate_limit_exceeded") => {
            "GPT-Live quota or rate limit reached"
        }
        _ => "GPT-Live request failed; check model, voice and account configuration",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fake {
        sent: Arc<Mutex<Vec<String>>>,
        tx: mpsc::Sender<String>,
        rx: mpsc::Receiver<String>,
    }
    impl Transport for Fake {
        fn send(&self, json: String) -> Result<()> {
            if json == protocol::close() {
                self.tx.try_send(
                    r#"{"type":"session.closed","reason":"close_requested","usage":{"seconds":3}}"#
                        .into(),
                )?;
            }
            self.sent.lock().unwrap().push(json);
            Ok(())
        }
        fn send_audio(&self, pcm: &[i16]) -> Result<()> {
            self.sent.lock().unwrap().push(serde_json::to_string(pcm)?);
            Ok(())
        }
        fn events(&mut self) -> &mut mpsc::Receiver<String> {
            &mut self.rx
        }
    }
    async fn make() -> (
        Session<Fake>,
        mpsc::Receiver<String>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let (tx, rx) = mpsc::channel(32);
        tx.try_send(r#"{"type":"session.started","session":{"model":"gpt-live-1"}}"#.into())
            .unwrap();
        let sent = Arc::new(Mutex::new(vec![]));
        let transport = Fake {
            sent: sent.clone(),
            tx,
            rx,
        };
        let (s, events) = Session::start(
            transport,
            protocol::SessionStart::new(
                "gpt-live-1",
                "marin",
                "gpt-5.6-luna",
                INSTRUCTIONS,
                BACKEND_INSTRUCTIONS,
            ),
            Instant::now(),
        )
        .await
        .unwrap();
        (s, events, sent)
    }
    fn transcript(delta: &str, at: u64) -> String {
        serde_json::json!({"type":"session.input_transcript.delta","delta":delta,"start_ms":at,"end_ms":at+100}).to_string()
    }

    #[tokio::test]
    async fn stop_phrase_survives_fragment_boundaries() {
        let (mut s, _, _) = make().await;
        assert_eq!(
            s.handle(&transcript("Nebu", 0)),
            Some(SessionEvent::UserActivity)
        );
        assert_eq!(
            s.handle(&transcript("la, st", 100)),
            Some(SessionEvent::UserActivity)
        );
        assert_eq!(
            s.handle(&transcript("op.", 200)),
            Some(SessionEvent::StopRequested)
        );
    }

    #[tokio::test]
    async fn stop_phrase_does_not_span_long_pauses_or_negation() {
        let (mut s, _, _) = make().await;
        s.handle(&transcript("Nebula", 0));
        assert_eq!(
            s.handle(&transcript(" stop", 5000)),
            Some(SessionEvent::UserActivity)
        );
        s.handle(&transcript("Nebula, don't", 10000));
        assert_eq!(
            s.handle(&transcript(" stop.", 10100)),
            Some(SessionEvent::UserActivity)
        );
    }

    #[tokio::test]
    async fn mute_sends_silence_and_requires_matching_acknowledgment() {
        let (mut s, _, sent) = make().await;
        s.set_muted(true).unwrap();
        s.send_microphone(&[0.5, -0.5], true).unwrap();
        assert_eq!(sent.lock().unwrap().last().unwrap(), "[0,0]");
        s.handle(r#"{"type":"session.input_audio.muted","client_event_id":"wrong"}"#);
        assert!(s.pending_mute.is_some());
        assert_eq!(s.handle(&transcript("Nebula stop.", 0)), None);
        s.handle(r#"{"type":"session.input_audio.muted","client_event_id":"nebula_mute_1"}"#);
        assert!(s.pending_mute.is_none());
        s.set_muted(false).unwrap();
        s.handle(r#"{"type":"session.input_audio.unmuted","client_event_id":"nebula_mute_2"}"#);
        assert!(s.pending_mute.is_none());
        s.send_microphone(&[0.5], false).unwrap();
        assert_ne!(sent.lock().unwrap().last().unwrap(), "[0]");
    }

    #[tokio::test]
    async fn stale_mute_acknowledgment_does_not_accept_new_command() {
        let (mut s, _, _) = make().await;
        s.set_muted(true).unwrap();
        s.set_muted(false).unwrap();
        s.handle(r#"{"type":"session.input_audio.muted","client_event_id":"nebula_mute_1"}"#);
        s.pending_mute.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(6);
        assert!(s.set_muted(false).is_err());
    }

    #[tokio::test]
    async fn full_duplex_audio_is_not_cancelled_by_user_transcripts() {
        let (mut s, _, sent) = make().await;
        assert!(matches!(
            s.handle(r#"{"type":"session.output_audio.delta","delta":"AAA="}"#),
            Some(SessionEvent::Audio(_))
        ));
        s.handle(&transcript("mm-hmm", 0));
        assert!(matches!(
            s.handle(r#"{"type":"session.output_audio.delta","delta":"AAA="}"#),
            Some(SessionEvent::Audio(_))
        ));
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn backend_completion_is_not_spoken_audio_completion() {
        let (mut s, _, _) = make().await;
        s.handle(r#"{"type":"response.event","event":{"type":"response.created","response":{"id":"r1"}}}"#);
        s.handle(r#"{"type":"response.event","event":{"type":"response.created","response":{"id":"r2"}}}"#);
        s.handle(r#"{"type":"response.event","event":{"type":"response.completed","response":{"id":"r1"}}}"#);
        assert!(s.backend_busy());
        s.handle(r#"{"type":"response.event","event":{"type":"response.completed","response":{"id":"r2"}}}"#);
        assert!(!s.backend_busy());
        assert!(matches!(
            s.handle(r#"{"type":"session.output_audio.delta","delta":"AAA="}"#),
            Some(SessionEvent::Audio(_))
        ));
    }

    #[tokio::test]
    async fn close_waits_for_final_usage_instead_of_summing_snapshots() {
        let (mut s, mut events, sent) = make().await;
        s.handle(r#"{"type":"session.usage.updated","usage":{"seconds":1}}"#);
        s.handle(r#"{"type":"session.usage.updated","usage":{"seconds":2}}"#);
        s.close(&mut events).await;
        assert_eq!(s.metrics.voice_seconds, Some(3.0));
        assert!(s.metrics.finalized);
        assert_eq!(sent.lock().unwrap().last().unwrap(), &protocol::close());
        assert!(s.send_microphone(&[0.0], false).is_err());
    }

    #[tokio::test]
    async fn errors_and_malformed_audio_do_not_echo_private_content() {
        let (mut s, _, _) = make().await;
        for raw in [
            r#"{"type":"error","error":{"code":null,"message":"sk-private"}}"#,
            r#"{"type":"session.output_audio.delta","delta":"private!"}"#,
            r#"{"type":"session.output_audio.delta","delta":"AA=="}"#,
        ] {
            let Some(SessionEvent::Failed(message)) = s.handle(raw) else {
                panic!("expected failure")
            };
            assert!(!message.contains("private"));
        }
    }

    #[tokio::test]
    async fn rolling_transcript_is_bounded_at_unicode_boundaries() {
        let (mut s, _, _) = make().await;
        s.handle(&transcript(&"é".repeat(300), 0));
        assert!(s.transcript.len() <= 512);
    }

    #[tokio::test]
    async fn realtime_acknowledgment_cannot_start_a_live_session() {
        let (tx, rx) = mpsc::channel(32);
        tx.try_send(r#"{"type":"session.updated"}"#.into()).unwrap();
        let transport = Fake {
            sent: Arc::new(Mutex::new(vec![])),
            tx,
            rx,
        };
        let start = protocol::SessionStart::new(
            "gpt-live-1",
            "marin",
            "gpt-5.6-luna",
            INSTRUCTIONS,
            BACKEND_INSTRUCTIONS,
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                Session::start(transport, start, Instant::now())
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_different_model_is_rejected_at_startup() {
        let (tx, rx) = mpsc::channel(32);
        tx.try_send(r#"{"type":"session.started","session":{"model":"gpt-realtime-2.1"}}"#.into())
            .unwrap();
        let transport = Fake {
            sent: Arc::new(Mutex::new(vec![])),
            tx,
            rx,
        };
        let start = protocol::SessionStart::new(
            "gpt-live-1",
            "marin",
            "gpt-5.6-luna",
            INSTRUCTIONS,
            BACKEND_INSTRUCTIONS,
        );
        assert!(
            Session::start(transport, start, Instant::now())
                .await
                .is_err()
        );
    }

    #[test]
    fn stop_command_requires_addressing_nebula_without_negation() {
        for text in [
            "stop",
            "Nebula, don't stop",
            "Nebula never stop",
            "Nebula stopping",
        ] {
            assert!(!is_stop_command(text));
        }
        for text in ["Nebula stop", "NEBULA, please stop!", "Hey Nebula stop."] {
            assert!(is_stop_command(text));
        }
    }
}
