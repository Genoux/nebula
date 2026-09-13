use serde::{Deserialize, Serialize};

pub const URL: &str = "wss://api.openai.com/v1/live/sessions";
pub const RATE: u32 = 24_000;

#[derive(Debug, Serialize)]
pub struct SessionStart {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub event_id: &'static str,
    pub session: SessionConfig,
}

#[derive(Debug, Serialize)]
pub struct SessionConfig {
    pub model: String,
    pub instructions: String,
    pub audio: Audio,
    pub delegation: Delegation,
    pub store: bool,
}

#[derive(Debug, Serialize)]
pub struct Audio {
    pub format: Format,
    pub output: Output,
}

#[derive(Debug, Serialize)]
pub struct Format {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub rate: u32,
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub voice: String,
}

#[derive(Debug, Serialize)]
pub struct Delegation {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub responses: Backend,
}

#[derive(Debug, Serialize)]
pub struct Backend {
    pub model: String,
    pub instructions: String,
    pub tools: Vec<serde_json::Value>,
    pub tool_choice: &'static str,
}

impl SessionStart {
    pub fn new(
        model: &str,
        voice: &str,
        backend: &str,
        instructions: &str,
        backend_instructions: &str,
    ) -> Self {
        Self {
            kind: "session.start",
            event_id: "nebula_start",
            session: SessionConfig {
                model: model.into(),
                instructions: instructions.into(),
                audio: Audio {
                    format: Format {
                        kind: "audio/pcm",
                        rate: RATE,
                    },
                    output: Output {
                        voice: voice.into(),
                    },
                },
                delegation: Delegation {
                    kind: "responses",
                    responses: Backend {
                        model: backend.into(),
                        instructions: backend_instructions.into(),
                        tools: vec![serde_json::json!({"type":"web_search"})],
                        tool_choice: "auto",
                    },
                },
                store: false,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session.started")]
    Started { session: StartedSession },
    #[serde(rename = "session.output_audio.delta")]
    Audio { delta: String },
    #[serde(rename = "session.output_transcript.delta")]
    OutputTranscript { delta: String },
    #[serde(rename = "session.input_transcript.delta")]
    InputTranscript {
        delta: String,
        start_ms: u64,
        end_ms: u64,
    },
    #[serde(rename = "response.event")]
    Response {
        event: Box<BackendEvent>,
        delegation_id: Option<String>,
    },
    #[serde(rename = "session.input_audio.muted")]
    Muted { client_event_id: String },
    #[serde(rename = "session.input_audio.unmuted")]
    Unmuted { client_event_id: String },
    #[serde(rename = "session.usage.updated")]
    Usage { usage: Usage },
    #[serde(rename = "session.closed")]
    Closed { usage: Usage, reason: String },
    #[serde(rename = "error")]
    Error { error: ApiError },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct StartedSession {
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct BackendEvent {
    pub response_id: Option<String>,
    pub item_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub response: Option<BackendResponse>,
    pub item: Option<BackendItem>,
}

#[derive(Debug, Deserialize)]
pub struct BackendItem {
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BackendResponse {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub seconds: f64,
}

#[derive(Debug, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub code: Option<String>,
}

pub fn append_audio(encoded: &str) -> String {
    serde_json::json!({"type":"session.input_audio.append", "audio":encoded}).to_string()
}

pub fn mute(muted: bool, event_id: &str) -> String {
    serde_json::json!({"type": if muted { "session.input_audio.mute" } else { "session.input_audio.unmute" }, "event_id":event_id}).to_string()
}

pub fn close() -> String {
    serde_json::json!({"type":"session.close"}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_start_has_shared_audio_and_a_web_search_backend() {
        let start = SessionStart::new("gpt-live-1", "marin", "gpt-5.6-luna", "voice", "reasoning");
        let actual = serde_json::to_value(start).unwrap();
        assert_eq!(
            actual,
            serde_json::json!({
                "type":"session.start", "event_id":"nebula_start",
                "session": {
                    "model":"gpt-live-1", "instructions":"voice", "store":false,
                    "audio":{"format":{"type":"audio/pcm","rate":24000},"output":{"voice":"marin"}},
                    "delegation":{"type":"responses","responses":{"model":"gpt-5.6-luna","instructions":"reasoning","tools":[{"type":"web_search"}],"tool_choice":"auto"}}
                }
            })
        );
    }

    #[test]
    fn unknown_events_are_forward_compatible() {
        assert!(matches!(
            serde_json::from_str::<ServerEvent>(r#"{"type":"future.event"}"#).unwrap(),
            ServerEvent::Other
        ));
    }

    #[test]
    fn live_audio_and_controls_use_live_event_names() {
        let value: serde_json::Value = serde_json::from_str(&append_audio("AAAA")).unwrap();
        assert_eq!(value["type"], "session.input_audio.append");
        let value: serde_json::Value = serde_json::from_str(&mute(true, "m1")).unwrap();
        assert_eq!(value["type"], "session.input_audio.mute");
        assert_eq!(value["event_id"], "m1");
        assert_eq!(close(), r#"{"type":"session.close"}"#);
    }
}
