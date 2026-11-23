use doso_sdk::rpc;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;
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
}

impl State {
    pub fn new(device: String) -> Self {
        Self {
            device,
            held: Arc::new(AtomicBool::new(false)),
            disconnects: Arc::new(AtomicUsize::new(0)),
            opens: Arc::new(AtomicUsize::new(0)),
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
                loop {
                    let request = tokio::select! {
                        _ = sender.closed() => break,
                        request = tokio::time::timeout(std::time::Duration::from_secs(3), requests.message()) =>
                            match request { Ok(Ok(Some(request))) => request, _ => break },
                    };
                    let (reason, final_output_confirmed, closing) = match request.action {
                        Some(rpc::teleop_request::Action::Input(_)) => {
                            ("SIMULATED stream open", false, false)
                        }
                        Some(rpc::teleop_request::Action::Claim(_)) => {
                            state.held.store(true, Ordering::SeqCst);
                            ("SIMULATED claim", false, false)
                        }
                        Some(rpc::teleop_request::Action::Hold(_)) => {
                            state.held.store(false, Ordering::SeqCst);
                            ("already_idle", false, false)
                        }
                        Some(rpc::teleop_request::Action::Close(_)) => {
                            state.held.store(false, Ordering::SeqCst);
                            ("already_idle", false, true)
                        }
                        _ => {
                            let _ = sender
                                .send(Err(Status::unimplemented("SIMULATED unsupported action")))
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
                        accepted: true,
                        reason: reason.into(),
                        input_window: 1,
                        final_output_confirmed,
                        control: Some(rpc::ControlState {
                            available: Some(true),
                            fresh: true,
                            held_by_client: Some(state.held.load(Ordering::SeqCst)),
                            ..Default::default()
                        }),
                        ..Default::default()
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
            if request.uri().path() != "/doso.v1.Motion/Teleop" {
                return Ok(unsupported());
            }
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default());
            Ok(grpc.streaming(service, request).await)
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
