use super::{Config, control_reply_from_sdk};
use anyhow::{Context, Result, ensure};
use doso_sdk::rpc as sdk_rpc;
use robot_cloud_common::control::rpc::{ControlReply, DeviceEvent, VelocityCommand, device_event};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tonic::{
    Request, Status,
    codegen::tokio_stream::wrappers::ReceiverStream,
    metadata::{Ascii, MetadataValue},
};

const TELEOP_CALL_TIMEOUT: Duration = Duration::from_secs(2);
const TELEOP_SEND_QUEUE: usize = 8;

pub(super) struct TeleopError {
    pub reason: String,
    pub submission_unknown: bool,
}

type BridgeResult<T> = std::result::Result<T, TeleopError>;

struct PendingTarget {
    cloud_sequence: u64,
    request_id: String,
    waiter: Option<oneshot::Sender<std::result::Result<sdk_rpc::TeleopReply, Status>>>,
}

/// Persistent local `Motion.Teleop` bridge preserving operator-supplied `input_window`.
pub(super) struct TeleopBridge {
    client_id: String,
    sender: Option<mpsc::Sender<sdk_rpc::TeleopRequest>>,
    reader: JoinHandle<()>,
    sequence: u64,
    latest_window: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<u64, PendingTarget>>>,
}

impl Drop for TeleopBridge {
    fn drop(&mut self) {
        self.healthy.store(false, Ordering::Release);
        self.sender.take();
        self.reader.abort();
    }
}

impl TeleopBridge {
    pub async fn open(
        config: &Config,
        session_id: &str,
        cloud_events: mpsc::Sender<DeviceEvent>,
    ) -> Result<Self> {
        let client_id = format!("cloud-{session_id}");
        let api_key: MetadataValue<Ascii> = config.local_api_key_metadata()?;
        let channel = config.local_channel_endpoint()?.connect().await?;
        let (sender, receiver) = mpsc::channel(TELEOP_SEND_QUEUE);
        let init_id = uuid::Uuid::new_v4().to_string();
        sender
            .send(sdk_rpc::TeleopRequest {
                command: Some(sdk_rpc::Command {
                    request_id: init_id.clone(),
                    client_id: client_id.clone(),
                }),
                sequence: 1,
                input_window: 0,
                action: Some(sdk_rpc::teleop_request::Action::Input(sdk_rpc::Empty {})),
            })
            .await
            .context("teleop_request_stream_closed")?;

        let mut request = Request::new(ReceiverStream::new(receiver));
        request.metadata_mut().insert("x-api-key", api_key);
        let mut replies = tokio::time::timeout(
            TELEOP_CALL_TIMEOUT,
            sdk_rpc::motion_client::MotionClient::new(channel).teleop(request),
        )
        .await
        .context("teleop_open_timeout")??
        .into_inner();

        let opened = tokio::time::timeout(TELEOP_CALL_TIMEOUT, replies.message())
            .await
            .context("teleop_open_reply_timeout")??
            .context("teleop_stream_ended_before_opening")?;
        ensure!(
            opened.request_id == init_id
                && opened.sequence == 1
                && opened.accepted
                && opened.input_window != 0,
            "robot_does_not_support_continuous_teleop"
        );

        let latest_window = Arc::new(AtomicU64::new(opened.input_window));
        let healthy = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let reader = tokio::spawn(run_reader(
            replies,
            session_id.to_owned(),
            cloud_events,
            latest_window.clone(),
            healthy.clone(),
            pending.clone(),
        ));

        Ok(Self {
            client_id,
            sender: Some(sender),
            reader,
            sequence: 1,
            latest_window,
            healthy,
            pending,
        })
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn latest_window(&self) -> u64 {
        self.latest_window.load(Ordering::Relaxed)
    }

    pub async fn claim(&mut self) -> BridgeResult<sdk_rpc::TeleopReply> {
        self.call(0, sdk_rpc::teleop_request::Action::Claim(sdk_rpc::Empty {}))
            .await
    }

    pub async fn hold(&mut self) -> BridgeResult<sdk_rpc::TeleopReply> {
        self.call(0, sdk_rpc::teleop_request::Action::Hold(sdk_rpc::Empty {}))
            .await
    }

    pub async fn close(mut self) -> BridgeResult<sdk_rpc::TeleopReply> {
        self.call(0, sdk_rpc::teleop_request::Action::Close(sdk_rpc::Empty {}))
            .await
    }

    pub fn publish_velocity(
        &mut self,
        cloud_sequence: u64,
        velocity: &VelocityCommand,
    ) -> BridgeResult<()> {
        if !self.is_healthy() {
            return Err(TeleopError {
                reason: "teleop_stream_closed".into(),
                submission_unknown: false,
            });
        }
        let Some(sender) = self.sender.as_ref() else {
            return Err(TeleopError {
                reason: "teleop_session_closed".into(),
                submission_unknown: false,
            });
        };
        let local_seq = self.sequence + 1;
        let request_id = uuid::Uuid::new_v4().to_string();
        self.pending.lock().unwrap().insert(
            local_seq,
            PendingTarget {
                cloud_sequence,
                request_id: request_id.clone(),
                waiter: None,
            },
        );
        // Preserve the operator-captured input window verbatim instead of overwriting with latest.
        let req = sdk_rpc::TeleopRequest {
            command: Some(sdk_rpc::Command {
                request_id,
                client_id: self.client_id.clone(),
            }),
            input_window: velocity.input_window,
            sequence: local_seq,
            action: Some(sdk_rpc::teleop_request::Action::Velocity(
                sdk_rpc::TeleopVelocity {
                    vx_m_s: velocity.vx_m_s,
                    vy_m_s: velocity.vy_m_s,
                    yaw_rad_s: velocity.yaw_rad_s,
                    deadman: velocity.deadman,
                    manual_mode: velocity.manual_mode,
                },
            )),
        };
        match sender.try_send(req) {
            Ok(()) => {
                self.sequence = local_seq;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.pending.lock().unwrap().remove(&local_seq);
                Err(TeleopError {
                    reason: "velocity_queue_full".into(),
                    submission_unknown: false,
                })
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.pending.lock().unwrap().remove(&local_seq);
                self.healthy.store(false, Ordering::Release);
                Err(TeleopError {
                    reason: "teleop_stream_closed".into(),
                    submission_unknown: false,
                })
            }
        }
    }

    async fn call(
        &mut self,
        cloud_sequence: u64,
        action: sdk_rpc::teleop_request::Action,
    ) -> BridgeResult<sdk_rpc::TeleopReply> {
        if !self.is_healthy() {
            return Err(TeleopError {
                reason: "teleop_stream_closed".into(),
                submission_unknown: false,
            });
        }
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| TeleopError {
                reason: "teleop_session_closed".into(),
                submission_unknown: false,
            })?
            .clone();
        let local_seq = self.sequence + 1;
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            local_seq,
            PendingTarget {
                cloud_sequence,
                request_id: request_id.clone(),
                waiter: Some(tx),
            },
        );
        self.sequence = local_seq;
        let req = sdk_rpc::TeleopRequest {
            command: Some(sdk_rpc::Command {
                request_id,
                client_id: self.client_id.clone(),
            }),
            sequence: local_seq,
            input_window: 0,
            action: Some(action),
        };
        if sender.send(req).await.is_err() {
            self.pending.lock().unwrap().remove(&local_seq);
            self.healthy.store(false, Ordering::Release);
            return Err(TeleopError {
                reason: "teleop_request_stream_closed".into(),
                submission_unknown: false,
            });
        }
        match tokio::time::timeout(TELEOP_CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(reply))) => Ok(reply),
            Ok(Ok(Err(status))) => Err(TeleopError {
                reason: status.message().to_owned(),
                submission_unknown: false,
            }),
            Ok(Err(_)) => Err(TeleopError {
                reason: "teleop_reply_stream_ended".into(),
                submission_unknown: true,
            }),
            Err(_) => {
                // Cancellation closes both sides so native authority cannot outlive a timed-out wait.
                self.pending.lock().unwrap().remove(&local_seq);
                self.healthy.store(false, Ordering::Release);
                self.sender.take();
                Err(TeleopError {
                    reason: "teleop_reply_timeout".into(),
                    submission_unknown: true,
                })
            }
        }
    }
}

async fn run_reader(
    mut replies: tonic::Streaming<sdk_rpc::TeleopReply>,
    session_id: String,
    cloud_events: mpsc::Sender<DeviceEvent>,
    latest_window: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<u64, PendingTarget>>>,
) {
    loop {
        match replies.message().await {
            Ok(Some(reply)) => {
                if reply.input_window != 0 {
                    latest_window.store(reply.input_window, Ordering::Relaxed);
                }
                if reply.sequence == 0
                    && reply.request_id.is_empty()
                    && reply.accepted
                    && reply.input_window != 0
                {
                    let update = ControlReply {
                        session_id: session_id.clone(),
                        sequence: 0,
                        accepted: true,
                        reason: "window_update".into(),
                        held_by_client: reply
                            .control
                            .filter(|c| c.fresh)
                            .and_then(|c| c.held_by_client),
                        final_output_confirmed: false,
                        submission_unknown: false,
                        input_window: reply.input_window,
                        ingress_accepted: false,
                    };
                    let _ = cloud_events.try_send(DeviceEvent {
                        event: Some(device_event::Event::Reply(update)),
                    });
                } else if reply.sequence != 0 && !reply.request_id.is_empty() {
                    let target = pending.lock().unwrap().remove(&reply.sequence);
                    let Some(target) = target else {
                        continue;
                    };
                    if reply.request_id != target.request_id {
                        fail_stream(
                            &healthy,
                            &pending,
                            &session_id,
                            &cloud_events,
                            Status::data_loss("teleop_reply_identity_mismatch"),
                            Some(target),
                        );
                        return;
                    }
                    match target.waiter {
                        Some(waiter) => {
                            let _ = waiter.send(Ok(reply));
                        }
                        None => {
                            let mapped =
                                control_reply_from_sdk(&session_id, target.cloud_sequence, reply);
                            let _ = cloud_events.try_send(DeviceEvent {
                                event: Some(device_event::Event::Reply(mapped)),
                            });
                        }
                    }
                } else {
                    fail_stream(
                        &healthy,
                        &pending,
                        &session_id,
                        &cloud_events,
                        Status::data_loss("invalid_teleop_reply_identity"),
                        None,
                    );
                    return;
                }
            }
            Ok(None) => {
                fail_stream(
                    &healthy,
                    &pending,
                    &session_id,
                    &cloud_events,
                    Status::unavailable("teleop_stream_ended"),
                    None,
                );
                return;
            }
            Err(status) => {
                fail_stream(&healthy, &pending, &session_id, &cloud_events, status, None);
                return;
            }
        }
    }
}

fn fail_stream(
    healthy: &AtomicBool,
    pending: &Mutex<HashMap<u64, PendingTarget>>,
    session_id: &str,
    cloud_events: &mpsc::Sender<DeviceEvent>,
    status: Status,
    extra: Option<PendingTarget>,
) {
    healthy.store(false, Ordering::Release);
    let mut drained: Vec<PendingTarget> = pending.lock().unwrap().drain().map(|(_, v)| v).collect();
    if let Some(target) = extra {
        drained.push(target);
    }
    for target in drained {
        match target.waiter {
            Some(waiter) => {
                let _ = waiter.send(Err(status.clone()));
            }
            None => {
                let _ = cloud_events.try_send(DeviceEvent {
                    event: Some(device_event::Event::Reply(ControlReply {
                        session_id: session_id.to_owned(),
                        sequence: target.cloud_sequence,
                        accepted: false,
                        reason: status.message().to_owned(),
                        held_by_client: None,
                        final_output_confirmed: false,
                        submission_unknown: true,
                        input_window: 0,
                        ingress_accepted: false,
                    })),
                });
            }
        }
    }
}
