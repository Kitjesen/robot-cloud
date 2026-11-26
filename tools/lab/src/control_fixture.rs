use doso_sdk::rpc;
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::{mpsc, watch};
use tonic::{
    Request, Response, Status,
    body::Body,
    codegen::{BoxFuture, Service, http, tokio_stream::wrappers::ReceiverStream},
    server::{NamedService, StreamingService, UnaryService},
};

#[derive(Clone)]
pub struct State {
    pub device: String,
    pub held: Arc<AtomicBool>,
    pub disconnects: Arc<AtomicUsize>,
    pub opens: Arc<AtomicUsize>,
    pub window: Arc<AtomicU64>,
    pub window_tx: watch::Sender<u64>,
    pub last_velocity_window: Arc<AtomicU64>,
    pub velocity_accepted: Arc<AtomicUsize>,
    pub velocity_rejected: Arc<AtomicUsize>,
    pub holds: Arc<AtomicUsize>,
    pub discrete_calls: Arc<Mutex<Vec<String>>>,
}

impl State {
    pub fn new(device: String) -> Self {
        let (window_tx, _) = watch::channel(1);
        Self {
            device,
            held: Arc::new(AtomicBool::new(false)),
            disconnects: Arc::new(AtomicUsize::new(0)),
            opens: Arc::new(AtomicUsize::new(0)),
            window: Arc::new(AtomicU64::new(1)),
            window_tx,
            last_velocity_window: Arc::new(AtomicU64::new(0)),
            velocity_accepted: Arc::new(AtomicUsize::new(0)),
            velocity_rejected: Arc::new(AtomicUsize::new(0)),
            holds: Arc::new(AtomicUsize::new(0)),
            discrete_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn authorize<T>(request: &Request<T>) -> Result<(), Status> {
    if request
        .metadata()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        != Some("lab-control-key")
    {
        return Err(Status::unauthenticated("SIMULATED SDK credential required"));
    }
    Ok(())
}

#[derive(Clone)]
pub struct System(pub State);

impl UnaryService<rpc::Handshake> for System {
    type Response = rpc::HandshakeReply;
    type Future = BoxFuture<Response<Self::Response>, Status>;
    fn call(&mut self, request: Request<rpc::Handshake>) -> Self::Future {
        let state = self.0.clone();
        Box::pin(async move {
            authorize(&request)?;
            Ok(Response::new(rpc::HandshakeReply {
                protocol_version: 2,
                info: Some(rpc::SystemInfo {
                    robot_id: Some(state.device),
                    ..Default::default()
                }),
            }))
        })
    }
}

impl NamedService for System {
    const NAME: &'static str = "doso.v1.System";
}

impl Service<http::Request<Body>> for System {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            if request.uri().path() != "/doso.v1.System/Handshake" {
                return Ok(unsupported());
            }
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default());
            Ok(grpc.unary(service, request).await)
        })
    }
}

#[derive(Clone)]
pub struct Motion(pub State);

struct Connection(State);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.held.store(false, Ordering::SeqCst);
        self.0.disconnects.fetch_add(1, Ordering::SeqCst);
    }
}

impl StreamingService<rpc::TeleopRequest> for Motion {
    type Response = rpc::TeleopReply;
    type ResponseStream = ReceiverStream<Result<Self::Response, Status>>;
    type Future = BoxFuture<Response<Self::ResponseStream>, Status>;
    fn call(&mut self, request: Request<tonic::Streaming<rpc::TeleopRequest>>) -> Self::Future {
        let state = self.0.clone();
        Box::pin(async move {
            authorize(&request)?;
            let mut requests = request.into_inner();
            state.opens.fetch_add(1, Ordering::SeqCst);
            let (sender, output) = mpsc::channel(4);
            tokio::spawn(async move {
                let _connection = Connection(state.clone());
                let mut window_rx = state.window_tx.subscribe();
                window_rx.borrow_and_update();
                loop {
                    let request = tokio::select! {
                        _ = sender.closed() => break,
                        changed = window_rx.changed() => {
                            if changed.is_ok() {
                                let new_window = *window_rx.borrow_and_update();
                                state.window.store(new_window, Ordering::SeqCst);
                                let update = rpc::TeleopReply {
                                    request_id: String::new(),
                                    sequence: 0,
                                    accepted: true,
                                    reason: "window_update".into(),
                                    input_window: new_window,
                                    ingress_accepted: false,
                                    final_output_confirmed: false,
                                    control: Some(rpc::ControlState {
                                        available: Some(true),
                                        fresh: true,
                                        held_by_client: Some(state.held.load(Ordering::SeqCst)),
                                        ..Default::default()
                                    }),
                                };
                                if sender.send(Ok(update)).await.is_err() {
                                    break;
                                }
                            }
                            continue;
                        }
                        request = tokio::time::timeout(std::time::Duration::from_secs(3), requests.message()) =>
                            match request { Ok(Ok(Some(request))) => request, _ => break },
                    };
                    let (accepted, ingress_accepted, reason, final_output_confirmed, closing) =
                        match request.action {
                            Some(rpc::teleop_request::Action::Input(_)) => {
                                (true, false, "SIMULATED stream open", false, false)
                            }
                            Some(rpc::teleop_request::Action::Claim(_)) => {
                                state.held.store(true, Ordering::SeqCst);
                                (true, false, "SIMULATED claim", false, false)
                            }
                            Some(rpc::teleop_request::Action::Hold(_)) => {
                                state.holds.fetch_add(1, Ordering::SeqCst);
                                state.held.store(false, Ordering::SeqCst);
                                (true, false, "already_idle", false, false)
                            }
                            Some(rpc::teleop_request::Action::Close(_)) => {
                                state.held.store(false, Ordering::SeqCst);
                                (true, false, "already_idle", false, true)
                            }
                            Some(rpc::teleop_request::Action::Velocity(_)) => {
                                state
                                    .last_velocity_window
                                    .store(request.input_window, Ordering::SeqCst);
                                let current = state.window.load(Ordering::SeqCst);
                                if request.input_window != current {
                                    state.velocity_rejected.fetch_add(1, Ordering::SeqCst);
                                    (false, false, "stale_input_window", false, false)
                                } else {
                                    state.velocity_accepted.fetch_add(1, Ordering::SeqCst);
                                    (true, true, "SIMULATED velocity", false, false)
                                }
                            }
                            _ => {
                                let _ = sender
                                    .send(Err(Status::unimplemented(
                                        "SIMULATED unsupported action",
                                    )))
                                    .await;
                                break;
                            }
                        };
                    let reply = rpc::TeleopReply {
                        request_id: request
                            .command
                            .map(|value| value.request_id)
                            .unwrap_or_default(),
                        sequence: request.sequence,
                        accepted,
                        reason: reason.into(),
                        input_window: state.window.load(Ordering::SeqCst),
                        ingress_accepted,
                        final_output_confirmed,
                        control: Some(rpc::ControlState {
                            available: Some(true),
                            fresh: true,
                            held_by_client: Some(state.held.load(Ordering::SeqCst)),
                            ..Default::default()
                        }),
                    };
                    if sender.send(Ok(reply)).await.is_err() || closing {
                        break;
                    }
                }
            });
            Ok(Response::new(ReceiverStream::new(output)))
        })
    }
}

impl NamedService for Motion {
    const NAME: &'static str = "doso.v1.Motion";
}

impl Service<http::Request<Body>> for Motion {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            let response = match request.uri().path() {
                "/doso.v1.Motion/Teleop" => {
                    tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .streaming(service, request)
                        .await
                }
                "/doso.v1.Motion/SwitchProfile" => {
                    tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(SwitchProfileCommand(service.0), request)
                        .await
                }
                path => {
                    const DISCRETE_METHODS: &[&str] = &[
                        "StandUp",
                        "SitDown",
                        "StopMotion",
                        "Stop",
                        "ClearEmergencyStop",
                        "Resume",
                        "EnableMotors",
                        "DisableMotors",
                    ];
                    if let Some(method) = DISCRETE_METHODS
                        .iter()
                        .copied()
                        .find(|&m| Some(m) == path.strip_prefix("/doso.v1.Motion/"))
                    {
                        tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                            .unary(DiscreteCommand(service.0, method), request)
                            .await
                    } else {
                        unsupported()
                    }
                }
            };
            Ok(response)
        })
    }
}

#[derive(Clone)]
struct DiscreteCommand(State, &'static str);

impl UnaryService<rpc::Command> for DiscreteCommand {
    type Response = rpc::Receipt;
    type Future = BoxFuture<Response<Self::Response>, Status>;
    fn call(&mut self, request: Request<rpc::Command>) -> Self::Future {
        let state = self.0.clone();
        let method = self.1;
        Box::pin(async move {
            authorize(&request)?;
            let cmd = request.into_inner();
            state.discrete_calls.lock().unwrap().push(method.to_owned());
            Ok(Response::new(rpc::Receipt {
                ok: true,
                accepted: Some(true),
                request_id: cmd.request_id,
                reason: format!("SIMULATED {method}"),
                reason_code: "ok".into(),
                ..Default::default()
            }))
        })
    }
}

#[derive(Clone)]
struct SwitchProfileCommand(State);

impl UnaryService<rpc::SwitchProfileRequest> for SwitchProfileCommand {
    type Response = rpc::Receipt;
    type Future = BoxFuture<Response<Self::Response>, Status>;
    fn call(&mut self, request: Request<rpc::SwitchProfileRequest>) -> Self::Future {
        let state = self.0.clone();
        Box::pin(async move {
            authorize(&request)?;
            let req = request.into_inner();
            state
                .discrete_calls
                .lock()
                .unwrap()
                .push(format!("SwitchProfile:{}", req.name));
            Ok(Response::new(rpc::Receipt {
                ok: true,
                accepted: Some(true),
                request_id: req.command.map(|c| c.request_id).unwrap_or_default(),
                reason: "SIMULATED SwitchProfile".into(),
                reason_code: "ok".into(),
                ..Default::default()
            }))
        })
    }
}

fn unsupported() -> http::Response<Body> {
    http::Response::builder()
        .header("grpc-status", "12")
        .header("content-type", "application/grpc")
        .body(Body::empty())
        .unwrap()
}
