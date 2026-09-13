use anyhow::{Context, Result, anyhow};
use nebula_audio::{Capture, LIVE_RATE, Playback, Queue, Tone};
use nebula_core::config::Config;
use nebula_core::event::{UiCommand, UiStatus};
use nebula_live::protocol::SessionStart;
use nebula_live::session::{self, Metrics, SessionEvent};
use nebula_live::{Session, Transport, WebSocketTransport};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// HUD refresh and idle-check resolution. 20Hz is smooth for bars and far
/// below anything that would load the loop.
const TICK: Duration = Duration::from_millis(50);
/// How long to wait for the device to start taking a reply before giving up on
/// measuring it.
const PLAYBACK_START_LIMIT: Duration = Duration::from_millis(500);

pub struct Controls<'a> {
    pub muted: Arc<AtomicBool>,
    pub commands: &'a mut mpsc::Receiver<UiCommand>,
    pub status: watch::Sender<UiStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    StopPhrase,
    IdleTimeout,
    EndCommand,
    CtrlC,
    ServerClosed,
}

impl EndReason {
    pub fn describe(self) -> &'static str {
        match self {
            EndReason::StopPhrase => "\"Nebula, stop\"",
            EndReason::IdleTimeout => "idle timeout",
            EndReason::EndCommand => "ended from the HUD",
            EndReason::CtrlC => "ctrl-c",
            EndReason::ServerClosed => "GPT-Live closed the session",
        }
    }
}

pub struct Outcome {
    pub metrics: Metrics,
    pub reason: EndReason,
}

pub struct Live<T: Transport = WebSocketTransport> {
    session: Session<T>,
    events: mpsc::Receiver<String>,
}

/// Open the socket and configure the session. Split from [`Live::run`] so the
/// daemon's state machine sees `Connected` when it actually happens.
pub async fn connect(config: &Config, api_key: &str, started: Instant) -> Result<Live> {
    let transport = WebSocketTransport::connect(api_key).await?;
    let mut update = SessionStart::new(
        &config.session.model,
        &config.session.voice,
        &config.session.backend_model,
        session::INSTRUCTIONS,
        session::BACKEND_INSTRUCTIONS,
    );
    if config.tools.enabled {
        update.session.delegation.responses.tools = crate::tools::definitions();
        update.session.instructions = session::INSTRUCTIONS.replace("no computer access", "local file, terminal, desktop, browser, SSH server and configured integration tools with local approval");
        update.session.instructions.push_str(" Delegate all system actions to the backend. Never claim an action succeeded without its tool result. Local approvals must be completed on the desktop; spoken yes is not approval. While approval is pending, briefly say to check the desktop, then wait.");
        update.session.delegation.responses.instructions =
            crate::tools::BACKEND_INSTRUCTIONS.into();
    }
    let (session, events) = Session::start(transport, update, started).await?;
    tracing::info!(
        "connected to GPT-Live ({}) in {:.0}ms",
        config.session.model,
        session.metrics.connect.unwrap_or_default().as_secs_f64() * 1000.0
    );
    Ok(Live { session, events })
}

impl<T: Transport> Live<T> {
    pub async fn run(
        mut self,
        config: &Config,
        speakers: Playback,
        controls: Controls<'_>,
    ) -> Result<Outcome> {
        let outcome = self.run_inner(config, speakers, controls).await;
        self.session.close(&mut self.events).await;
        outcome.map(|mut outcome| {
            outcome.metrics = self.session.metrics.clone();
            outcome
        })
    }

    async fn run_inner(
        &mut self,
        config: &Config,
        speakers: Playback,
        controls: Controls<'_>,
    ) -> Result<Outcome> {
        let Controls {
            muted,
            commands,
            status,
        } = controls;
        let mic = Capture::open(
            LIVE_RATE,
            &config.audio.input_device,
            config.audio.buffer_frames,
        )
        .context("could not open the microphone")?;
        let mic_level = mic.level();
        let queue = speakers.queue();

        let mut mic_tick = tokio::time::interval(Duration::from_millis(5));
        let mut mic_last_received = Instant::now();
        let (timing_tx, mut timings) = mpsc::unbounded_channel::<Duration>();
        let ctrl_c = crate::shutdown_signal();
        tokio::pin!(ctrl_c);
        let mut ticker = tokio::time::interval(TICK);
        let idle_timeout = config.idle_timeout();
        let mut last_activity = Instant::now();
        let mut speech_display = crate::speech_display::SpeechDisplay::default();
        let mut reply_started = false;
        let (tool_activity_tx, tool_activity) = watch::channel(None);
        let executor = crate::tools::Executor::new(config.tools.clone(), tool_activity_tx);
        let mut tool_queue = std::collections::VecDeque::<session::ToolBatch>::new();
        let mut tool_task =
            tokio::task::JoinSet::<(session::ToolBatch, Vec<serde_json::Value>)>::new();

        let reason = loop {
            tokio::select! {
                _ = mic_tick.tick() => {
                    for _ in 0..16 {
                        let Some(chunk) = mic.try_recv() else { break; };
                        mic_last_received = Instant::now();
                        let is_muted = muted.load(Ordering::Relaxed);
                        self.session.set_muted(is_muted)?;
                        self.session.send_microphone(&chunk, is_muted)?;
                    }
                    if mic_last_received.elapsed() > Duration::from_secs(3) {
                        return Err(anyhow!("the microphone stopped delivering audio"));
                    }
                }

                raw = self.events.recv() => {
                    let Some(raw) = raw else {
                        return Err(anyhow!("the GPT-Live connection closed"));
                    };
                    let Some(event) = self.session.handle(&raw) else { continue };
                    match event {
                        SessionEvent::Audio(pcm) => {
                            if !reply_started && pcm.iter().any(|sample| sample.abs() > 0.005) {
                                reply_started = true;
                                measure_playback_start(Arc::clone(&queue), timing_tx.clone());
                            }
                            if queue.queued() + pcm.len() as u64 > LIVE_RATE as u64 * 2 {
                                return Err(anyhow!("assistant playback backlog exceeded two seconds"));
                            }
                            queue.push(&pcm);
                        }
                        SessionEvent::UserActivity => {
                            last_activity = Instant::now();
                            speech_display.user_text(Instant::now());
                        }
                        SessionEvent::AssistantActivity => { speech_display.output_text(Instant::now()); }
                        SessionEvent::ToolCalls(batch) => { tool_queue.push_back(batch); last_activity = Instant::now(); }
                        SessionEvent::BackendActivity => {}
                        SessionEvent::Closed => break EndReason::ServerClosed,
                        SessionEvent::StopRequested => break EndReason::StopPhrase,
                        SessionEvent::Failed(message) => return Err(anyhow!(message)),
                    }
                }

                Some(command) = commands.recv() => {
                    if command == UiCommand::EndSession {
                        break EndReason::EndCommand;
                    }
                }

                Some(completed) = tool_task.join_next(), if !tool_task.is_empty() => {
                    let (batch, results) = completed.context("local tool worker failed")?;
                    self.session.submit_tool_results(&batch, &results)?;
                    last_activity = Instant::now();
                }

                Some(took) = timings.recv() => {
                    tracing::info!("first audio -> played {:.0}ms", took.as_secs_f64() * 1000.0);
                    self.session.metrics.first_audio_to_playback = Some(took);
                }

                _ = ticker.tick() => {
                    if queue.level() > 0.005 {
                        last_activity = Instant::now();

                    }
                    speech_display.audio(queue.level(), Instant::now());
                    let speaking = speech_display.speaking(Instant::now());
                    if !speaking { reply_started = false; }
                    if tool_task.is_empty() && let Some(batch) = tool_queue.pop_front() {
                        let executor = executor.clone();
                        tool_task.spawn(async move {
                            let mut results = Vec::new();
                            for call in &batch.calls { results.push(executor.execute(&call.name, &call.arguments).await); }
                            (batch, results)
                        });
                    }
                    let working = !tool_task.is_empty() || !tool_queue.is_empty();
                    if working { last_activity = Instant::now(); }
                    status.send_replace(live_status(&queue, speaking, working || self.session.backend_busy(), mic_level.peak()).with_activity((*tool_activity.borrow()).or(self.session.activity())));
                    if last_activity.elapsed() > idle_timeout {
                        break EndReason::IdleTimeout;
                    }
                }

                _ = &mut ctrl_c => break EndReason::CtrlC,
            }
        };

        tool_task.shutdown().await;
        queue.clear();
        queue.push(&Tone::Disconnect.render(LIVE_RATE, 0.3));
        let fading = Instant::now();
        while !queue.is_idle() && fading.elapsed() < Duration::from_millis(600) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok(Outcome {
            metrics: self.session.metrics.clone(),
            reason,
        })
    }
}

fn live_status(queue: &Queue, speaking: bool, thinking: bool, mic_peak: f32) -> UiStatus {
    if speaking {
        UiStatus::Speaking {
            level: queue.level(),
        }
    } else if thinking {
        UiStatus::Thinking
    } else {
        UiStatus::Listening {
            level: mic_peak.min(1.0),
        }
    }
}

/// Time from the first delta of a reply being queued to the device actually
/// taking samples from it. Polled at 1ms in its own task so the measurement's
/// resolution does not depend on how busy the conversation loop is.
fn measure_playback_start(queue: Arc<Queue>, report: mpsc::UnboundedSender<Duration>) {
    let queued_at = Instant::now();
    let before = queue.consumed();
    tokio::spawn(async move {
        while queued_at.elapsed() < PLAYBACK_START_LIMIT {
            if queue.consumed() > before {
                let _ = report.send(queued_at.elapsed());
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
}

pub async fn check_connection(
    config: &Config,
    api_key: &str,
    web_search: bool,
    tool_probe: bool,
) -> Result<()> {
    let mut probe_config = config.clone();
    // Diagnostic sessions expose only discovery; they must never execute model-requested actions.
    probe_config.tools.enabled = tool_probe;
    let mut live = connect(&probe_config, api_key, Instant::now()).await?;
    let (activity, _) = watch::channel(None);
    let executor = crate::tools::Executor::new(probe_config.tools, activity);
    let mut tool_completed = false;
    let result = async {
        live.session.request_reasoning(if tool_probe { "Call tools_list once to verify local function execution. Then briefly report that local tooling is available. Do not call any other function." } else if web_search {
            "Use web search to find the latest stable Rust release announcement on the official Rust blog. Return the title and source URL."
        } else { "Calculate 137 times 29 and return the number." })?;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let silence = vec![0.0; LIVE_RATE as usize / 50];
        let shutdown = crate::shutdown_signal();
        tokio::pin!(shutdown);
        let mut backend_started = false;
        let mut backend_finished = false;
        tokio::time::timeout(Duration::from_secs(if web_search { 60 } else { 30 }), async {
            loop {
                tokio::select! {
                    _ = tick.tick() => live.session.send_microphone(&silence, false)?,
                    raw = live.events.recv() => {
                        let raw = raw.context("GPT-Live closed during connection test")?;
                        match live.session.handle(&raw) {
                            Some(SessionEvent::Audio(samples)) if backend_finished && (!tool_probe || tool_completed) && (!web_search || live.session.web_search_completed()) && samples.iter().any(|sample| sample.abs() > 0.005) => return Ok::<(), anyhow::Error>(()),
                            Some(SessionEvent::ToolCalls(batch)) => {
                                let mut results = Vec::new();
                                for call in &batch.calls {
                                    if tool_probe && call.name=="tools_list" {
                                        let result = executor.execute(&call.name, &call.arguments).await;
                                        tool_completed |= result["ok"]==true;
                                        results.push(result);
                                    } else { results.push(serde_json::json!({"ok":false,"error":"diagnostic permits only tools_list"})); }
                                }
                                live.session.submit_tool_results(&batch, &results)?;
                                backend_finished = false;
                            }
                            Some(SessionEvent::BackendActivity) => {
                                if live.session.backend_busy() { backend_started = true; }
                                else if backend_started {
                                    backend_finished = true;
                                    live.session.append_instructions("The connection test is complete. Briefly say Nebula is ready now, then remain quiet.")?;
                                }
                            }
                            Some(SessionEvent::Failed(message)) => return Err(anyhow!(message)),
                            Some(SessionEvent::Closed) => return Err(anyhow!("GPT-Live ended before returning audio")),
                            _ => {}
                        }
                    }
                    _ = &mut shutdown => return Err(anyhow!("connection test interrupted")),
                }
            }
        }).await.context("GPT-Live did not complete the requested backend and audio checks before the deadline")?
    }.await;
    live.session.close(&mut live.events).await;
    result?;
    if !live.session.metrics.finalized {
        return Err(anyhow!("GPT-Live did not confirm finalization"));
    }
    println!(
        "GPT-Live startup, reasoning backend, generated audio and graceful close passed; no microphone audio sent"
    );
    if tool_probe {
        println!("Local function execution, result submission and backend continuation passed");
    }
    if web_search {
        println!("Web search completed successfully through the reasoning backend");
    }
    println!("{}", live.session.metrics.report());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeTransport {
        incoming: mpsc::Receiver<String>,
        sent: Arc<Mutex<Vec<String>>>,
        microphone_samples: Arc<AtomicUsize>,
        closed: Arc<AtomicBool>,
        final_events: mpsc::Sender<String>,
    }

    impl Transport for FakeTransport {
        fn send(&self, json: String) -> Result<()> {
            if json.contains("session.close") {
                self.final_events.try_send(r#"{"type":"session.closed","reason":"close_requested","usage":{"seconds":1}}"#.into()).unwrap();
            }
            self.sent.lock().unwrap().push(json);
            Ok(())
        }
        fn send_audio(&self, pcm: &[i16]) -> Result<()> {
            self.microphone_samples
                .fetch_add(pcm.len(), Ordering::Relaxed);
            Ok(())
        }
        fn events(&mut self) -> &mut mpsc::Receiver<String> {
            &mut self.incoming
        }
        fn close(
            &mut self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.closed.store(true, Ordering::Relaxed);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    #[ignore = "requires working PipeWire input and output devices"]
    async fn hardware_live_lifecycle_with_mock_live() {
        for stop_phrase in [false, true] {
            let (events_tx, incoming) = mpsc::channel(32);
            events_tx
                .send(r#"{"type":"session.started","session":{"model":"gpt-live-1"}}"#.into())
                .await
                .unwrap();
            let sent = Arc::new(Mutex::new(Vec::new()));
            let samples = Arc::new(AtomicUsize::new(0));
            let closed = Arc::new(AtomicBool::new(false));
            let transport = FakeTransport {
                incoming,
                sent: sent.clone(),
                microphone_samples: samples.clone(),
                closed: closed.clone(),
                final_events: events_tx.clone(),
            };
            let (session, events) = Session::start(
                transport,
                SessionStart::new(
                    "gpt-live-1",
                    "marin",
                    "gpt-5.6-luna",
                    session::INSTRUCTIONS,
                    session::BACKEND_INSTRUCTIONS,
                ),
                Instant::now(),
            )
            .await
            .unwrap();
            let mut config = Config::default();
            config.session.idle_timeout_seconds = 1;
            let speakers = Playback::open(LIVE_RATE, "default", 240).unwrap();
            let (status, _) = watch::channel(UiStatus::Connecting);
            let (_commands, mut commands) = mpsc::channel(32);
            let server = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if stop_phrase {
                    events_tx.send(r#"{"type":"session.input_transcript.delta","delta":"Nebula, stop.","start_ms":0,"end_ms":100}"#.into()).await.unwrap();
                } else {
                    events_tx
                        .send(r#"{"type":"session.input_transcript.delta","delta":"hello","start_ms":0,"end_ms":100}"#.into())
                        .await
                        .unwrap();
                }
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if events_tx
                        .send(r#"{"type":"session.output_audio.delta","delta":"AAA="}"#.into())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let live = Live { session, events };
            let outcome = tokio::time::timeout(
                Duration::from_secs(4),
                live.run(
                    &config,
                    speakers,
                    Controls {
                        muted: Arc::new(AtomicBool::new(false)),
                        commands: &mut commands,
                        status,
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();
            server.abort();
            assert_eq!(
                outcome.reason,
                if stop_phrase {
                    EndReason::StopPhrase
                } else {
                    EndReason::IdleTimeout
                }
            );
            assert!(closed.load(Ordering::Relaxed));
            assert!(samples.load(Ordering::Relaxed) > 0);
        }
    }
}
