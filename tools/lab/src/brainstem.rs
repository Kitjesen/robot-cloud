use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};
use tonic::{
    Request, Response, Status,
    body::Body,
    codegen::{BoxFuture, Service, http, tokio_stream::wrappers::TcpListenerStream},
    server::{NamedService, UnaryService},
};

#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
struct State {
    #[prost(int32, tag = "1")]
    kind: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ControlStatus {
    #[prost(message, optional, tag = "1")]
    fsm: Option<State>,
}

#[derive(Clone)]
struct RobotControl(Arc<Source>);

struct Source {
    reads: AtomicUsize,
    kind: AtomicI32,
    failed: AtomicBool,
}

impl UnaryService<Empty> for RobotControl {
    type Response = ControlStatus;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, _: Request<Empty>) -> Self::Future {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        let source = self.0.clone();
        Box::pin(async move {
            if source.failed.load(Ordering::SeqCst) {
                return Err(Status::unavailable("SIMULATED read failure"));
            }
            // SIMULATED state, with false/NONE defaults for the control fields.
            Ok(Response::new(ControlStatus {
                fsm: Some(State {
                    kind: source.kind.load(Ordering::SeqCst),
                }),
            }))
        })
    }
}

impl Service<http::Request<Body>> for RobotControl {
    type Response = http::Response<Body>;
    type Error = std::convert::Infallible;
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
            if request.method() != http::Method::POST
                || request.uri().path() != "/brainstem.api.v1.RobotControl/GetControlStatus"
            {
                let mut response = http::Response::new(Body::empty());
                response
                    .headers_mut()
                    .insert("grpc-status", "12".parse().unwrap());
                response
                    .headers_mut()
                    .insert("content-type", "application/grpc".parse().unwrap());
                return Ok(response);
            }
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default());
            Ok(grpc.unary(service, request).await)
        })
    }
}

impl NamedService for RobotControl {
    const NAME: &'static str = "brainstem.api.v1.RobotControl";
}

pub struct Brainstem {
    source: Arc<Source>,
    task: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Brainstem {
    pub async fn start() -> anyhow::Result<Self> {
        // Bind before any cloud link starts, so an existing service can never be read.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:13146").await?;
        let source = Arc::new(Source {
            reads: AtomicUsize::new(0),
            kind: AtomicI32::new(1),
            failed: AtomicBool::new(false),
        });
        let server = tonic::transport::Server::builder()
            .add_service(RobotControl(source.clone()))
            .serve_with_incoming(TcpListenerStream::new(listener));
        Ok(Self {
            source,
            task: tokio::spawn(server),
        })
    }

    pub fn reads(&self) -> usize {
        self.source.reads.load(Ordering::SeqCst)
    }

    pub fn set_state(&self, kind: i32) {
        self.source.kind.store(kind, Ordering::SeqCst);
    }

    pub fn fail(&self, failed: bool) {
        self.source.failed.store(failed, Ordering::SeqCst);
    }

    pub fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.task.is_finished(),
            "SIMULATED Brainstem stopped unexpectedly"
        );
        Ok(())
    }
}

impl Drop for Brainstem {
    fn drop(&mut self) {
        self.task.abort();
        println!("SIMULATED GetControlStatus reads={}", self.reads());
    }
}
