use super::Hub;
use robot_cloud_common::{
    MAX_MESSAGE_BYTES,
    control::{PROOF_MS, Proof, rpc},
    decode_signed, is_uuid, now_ms,
};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
};
use tokio_stream::{
    Stream,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

type DeviceStream = Pin<Box<dyn Stream<Item = Result<rpc::DeviceCommand, Status>> + Send>>;
type OperatorStream = Pin<Box<dyn Stream<Item = Result<rpc::ControlReply, Status>> + Send>>;

#[derive(Clone)]
struct Relay(Arc<Hub>);

struct Shutdown(Arc<Hub>);

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

#[tonic::async_trait]
impl rpc::remote_control_server::RemoteControl for Relay {
    type DeviceLinkStream = DeviceStream;
    type OperateStream = OperatorStream;

    async fn device_link(
        &self,
        request: Request<Streaming<rpc::DeviceEvent>>,
    ) -> Result<Response<Self::DeviceLinkStream>, Status> {
        let device_id = request
            .metadata()
            .get("x-doso-device-id")
            .and_then(|value| value.to_str().ok())
            .filter(|id| is_uuid(id))
            .ok_or_else(|| Status::unauthenticated("device_identity_required"))?
            .to_owned();
        let key = self
            .0
            .gateway
            .device_key(&device_id)
            .map_err(|_| Status::permission_denied("device_disabled"))?;
        let permit = self
            .0
            .authenticating
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("device_authentication_busy"))?;
        let challenge = Uuid::new_v4().to_string();
        let (commands, output) = mpsc::channel(8);
        commands
            .try_send(Ok(rpc::DeviceCommand {
                command: Some(rpc::device_command::Command::Challenge(challenge.clone())),
            }))
            .map_err(|_| Status::internal("device_stream_unavailable"))?;
        let hub = self.0.clone();
        let mut input = request.into_inner();
        tokio::spawn(async move {
            let result = async {
                let event =
                    tokio::time::timeout(Duration::from_millis(PROOF_MS as u64), input.message())
                        .await
                        .map_err(|_| Status::deadline_exceeded("device_proof_timeout"))??
                        .ok_or_else(|| Status::unauthenticated("device_proof_required"))?;
                let Some(rpc::device_event::Event::Proof(bytes)) = event.event else {
                    return Err(Status::unauthenticated("device_proof_required"));
                };
                let proof: Proof = decode_signed(&bytes, &key)
                    .map_err(|_| Status::unauthenticated("invalid_device_proof"))?;
                proof
                    .validate(&challenge, now_ms())
                    .map_err(|_| Status::unauthenticated("invalid_device_proof"))?;
                if proof.device_id != device_id {
                    return Err(Status::permission_denied("device_identity_mismatch"));
                }
                let (stop, stopped) = watch::channel(false);
                hub.register(
                    &device_id,
                    &proof.connection_id,
                    key,
                    commands.clone(),
                    stop,
                )?;
                drop(permit);
                let result = device_events(
                    &hub,
                    &device_id,
                    &proof.connection_id,
                    &mut input,
                    &commands,
                    stopped,
                )
                .await;
                hub.disconnected(&device_id, &proof.connection_id);
                result
            }
            .await;
            if let Err(error) = result {
                let _ = commands.try_send(Err(error));
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(output))))
    }

    async fn operate(
        &self,
        request: Request<Streaming<rpc::OperatorInput>>,
    ) -> Result<Response<Self::OperateStream>, Status> {
        let mut input = request.into_inner();
        let first = tokio::time::timeout(Duration::from_secs(10), input.message())
            .await
            .map_err(|_| Status::deadline_exceeded("control_grant_timeout"))??
            .ok_or_else(|| Status::unauthenticated("control_grant_required"))?;
        let Some(rpc::operator_input::Input::Grant(bytes)) = first.input else {
            return Err(Status::unauthenticated("control_grant_required"));
        };
        let (replies, output) = mpsc::channel(8);
        let (grant, mut ended) = self.0.attach(&bytes, replies.clone())?;
        replies
            .try_send(Ok(rpc::ControlReply {
                session_id: grant.session_id.clone(),
                sequence: 0,
                accepted: true,
                reason: "session_open".into(),
                ..Default::default()
            }))
            .map_err(|_| Status::internal("operator_stream_unavailable"))?;
        let hub = self.0.clone();
        tokio::spawn(async move {
            let result = async {
                loop {
                    tokio::select! {
                        _ = replies.closed() => return Ok(()),
                        _ = ended.changed() => return Ok(()),
                        event = input.message() => {
                            let Some(event) = event? else { return Ok(()); };
                            let Some(rpc::operator_input::Input::Command(command)) = event.input else {
                                return Err(Status::invalid_argument("control_command_required"));
                            };
                            hub.input(&grant, command)?;
                        }
                    }
                }
            }.await;
            hub.detach(&grant);
            if let Err(error) = result {
                let _ = replies.try_send(Err(error));
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(output))))
    }
}

async fn device_events(
    hub: &Hub,
    device_id: &str,
    connection_id: &str,
    input: &mut Streaming<rpc::DeviceEvent>,
    commands: &mpsc::Sender<Result<rpc::DeviceCommand, Status>>,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), Status> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = commands.closed() => return Ok(()),
            _ = stopped.changed() => return Ok(()),
            _ = interval.tick() => if !hub.maintain(device_id, connection_id) { return Ok(()); },
            event = input.message() => {
                let Some(event) = event? else { return Ok(()); };
                let Some(rpc::device_event::Event::Reply(reply)) = event.event else {
                    return Err(Status::invalid_argument("device_reply_required"));
                };
                hub.reply(device_id, connection_id, reply)?;
            }
        }
    }
}

pub async fn serve(
    hub: Arc<Hub>,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), tonic::transport::Error> {
    let _cleanup = Shutdown(hub.clone());
    let service = rpc::remote_control_server::RemoteControlServer::new(Relay(hub.clone()))
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    tonic::transport::Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
            while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
            hub.shutdown();
        })
        .await
}
