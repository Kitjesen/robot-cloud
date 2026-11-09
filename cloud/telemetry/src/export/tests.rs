use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
};
use std::sync::{Arc, Mutex};

const DEVICE: &str = "11111111-1111-4111-8111-111111111111";
const PLATFORM: &str = "22222222-2222-4222-8222-222222222222";
const TOKEN: &str = "33333333-3333-4333-8333-333333333333";
const OWNER: &str = "44444444-4444-4444-8444-444444444444";
const OBSERVED: &str = "2026-11-03T12:00:00.000Z";

fn item(error: bool) -> Value {
    let mut report = json!({"v":1,"kind":"status.report","deviceId":DEVICE,
        "connectionId":TOKEN,"reportingSessionId":PLATFORM,"authorizationEpoch":3,
        "consentVersion":4,"seq":7,"observedAt":OBSERVED});
    if error {
        report["error"] = json!("upstream_unavailable");
    } else {
        report["status"] = json!({"state":{"kind":"CMS_STATE_KIND_GROUNDED","transition":"CMS_TRANSITION_KIND_NONE"},
            "control":{"owner":"CONTROL_OWNER_YUNZHUO","motorOutputEnabled":false,"criticalMotorFault":false},
            "transport":"local-insecure","location":{"lat":31.2,"lon":121.4,"fixType":1,"observedAt":OBSERVED,"accuracyM":null}});
    }
    json!({"id":TOKEN,"leaseToken":TOKEN,"deviceId":DEVICE,"ownerId":OWNER,"simulated":true,
        "authorizationEpoch":3,"consentVersion":4,"tbDeviceId":null,
        "received":{"report":report,"receivedAt":"2026-11-03T12:00:00.100Z"}})
}

#[test]
fn keeps_source_time_and_complete_report_without_lease_expiry_check() {
    let original = item(false);
    let record: Item = serde_json::from_value(original.clone()).unwrap();
    record.validate().unwrap();
    let value = projection(&record).unwrap();
    assert_eq!(
        value["ts"],
        json!(robot_cloud_common::parse_time(OBSERVED).unwrap())
    );
    assert_eq!(value["values"]["controlOwner"], "CONTROL_OWNER_YUNZHUO");
    assert_eq!(value["values"]["stateKind"], "CMS_STATE_KIND_GROUNDED");
    assert_eq!(value["values"]["sampleOk"], true);
    assert_eq!(value["values"]["readError"], "");
    let source: Value =
        serde_json::from_str(value["values"]["sourceReport"].as_str().unwrap()).unwrap();
    assert_eq!(source, original["received"]["report"]);
    assert!(source["status"]["location"].is_object());
}

#[test]
fn failed_sample_has_no_fabricated_status() {
    let record: Item = serde_json::from_value(item(true)).unwrap();
    let value = projection(&record).unwrap();
    assert_eq!(value["values"]["sampleOk"], false);
    assert_eq!(value["values"]["readError"], "upstream_unavailable");
    for key in [
        "stateKind",
        "controlOwner",
        "motorOutputEnabled",
        "criticalMotorFault",
    ] {
        assert!(value["values"].get(key).is_none());
    }
}

#[derive(Default)]
struct Calls {
    actions: Vec<Value>,
    writes: Vec<Value>,
    gets: Vec<String>,
    fail_write: bool,
    conflict: bool,
    revoked: bool,
    existing: bool,
    same_sample: bool,
    claimed: bool,
}

type Shared = Arc<Mutex<Calls>>;

async fn source(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["apikey"], "s".repeat(32));
    let mut state = state.lock().unwrap();
    state.actions.push(body.clone());
    Json(match body["action"].as_str().unwrap() {
        "claim" => {
            if state.claimed {
                json!({"v":1,"items":[]})
            } else {
                state.claimed = true;
                json!({"v":1,"items":[item(false)]})
            }
        }
        "check" => {
            if !body["tbDeviceId"].is_null() {
                assert_eq!(body["tbDeviceId"], PLATFORM);
            }
            json!({"v":1,"allowed":!state.revoked})
        }
        "finish" => json!({"v":1,"accepted":true}),
        _ => panic!("unexpected action"),
    })
}

async fn platform_get(
    State(state): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
) -> (StatusCode, Json<Value>) {
    assert_eq!(
        headers["x-authorization"],
        format!("ApiKey {}", "t".repeat(32))
    );
    let mut state = state.lock().unwrap();
    state.gets.push(uri.to_string());
    if uri.path() == "/api/tenant/device" {
        return if state.existing {
            (
                StatusCode::OK,
                Json(json!({"name":format!("doso:{DEVICE}:3"),"id":{"id":PLATFORM}})),
            )
        } else {
            (StatusCode::NOT_FOUND, Json(json!({})))
        };
    }
    let query = reqwest::Url::parse(&format!("http://127.0.0.1{uri}")).unwrap();
    let start: i64 = query
        .query_pairs()
        .find(|(key, _)| key == "startTs")
        .unwrap()
        .1
        .parse()
        .unwrap();
    let end: i64 = query
        .query_pairs()
        .find(|(key, _)| key == "endTs")
        .unwrap()
        .1
        .parse()
        .unwrap();
    let timestamp = robot_cloud_common::parse_time(OBSERVED).unwrap();
    // ThingsBoard 4.3.1.8 reads [startTs, endTs); an exclusive end at this sample omits it.
    if timestamp < start || timestamp >= end {
        return (StatusCode::OK, Json(json!({})));
    }
    if state.conflict {
        (
            StatusCode::OK,
            Json(
                json!({"seq":[{"ts":robot_cloud_common::parse_time(OBSERVED).unwrap(),"value":"99"}]}),
            ),
        )
    } else if state.same_sample {
        let record: Item = serde_json::from_value(item(false)).unwrap();
        let value = projection(&record).unwrap();
        let mut history = json!({});
        for key in [
            "seq",
            "connectionId",
            "reportingSessionId",
            "authorizationEpoch",
            "consentVersion",
        ] {
            let source = &value["values"][key];
            let source = source
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| source.to_string());
            history[key] = json!([{"ts":value["ts"],"value":source}]);
        }
        (StatusCode::OK, Json(history))
    } else {
        (StatusCode::OK, Json(json!({})))
    }
}

async fn platform_post(
    State(state): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(
        headers["x-authorization"],
        format!("ApiKey {}", "t".repeat(32))
    );
    let mut state = state.lock().unwrap();
    state.writes.push(json!({"path":uri.path(),"body":body}));
    if uri.path() == "/api/device" {
        state.existing = true;
        return (
            StatusCode::OK,
            Json(json!({"name":format!("doso:{DEVICE}:3"),"id":{"id":PLATFORM}})),
        );
    }
    if uri.path().ends_with("/timeseries/ANY") && state.fail_write {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})))
    } else {
        (StatusCode::OK, Json(json!({})))
    }
}

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(router: Router) -> Server {
    let router = router.layer(axum::middleware::map_response(
        async |mut response: axum::response::Response| {
            response
                .headers_mut()
                .insert("connection", "close".parse().unwrap());
            response
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { url, task }
}

async fn setup(calls: Shared) -> (Server, Server, Worker) {
    let source = server(
        Router::new()
            .route("/", post(source))
            .with_state(calls.clone()),
    )
    .await;
    let platform = server(
        Router::new()
            .route(
                "/{*path}",
                axum::routing::get(platform_get).post(platform_post),
            )
            .with_state(calls),
    )
    .await;
    let worker = Worker::new(&source.url, &"s".repeat(32), &platform.url, &"t".repeat(32)).unwrap();
    (source, platform, worker)
}

#[tokio::test]
async fn delivers_through_mapping_check_and_confirms_only_after_write() {
    let calls = Shared::default();
    let (_source, _platform, worker) = setup(calls.clone()).await;
    assert!(worker.step().await.unwrap());
    assert!(!worker.step().await.unwrap());
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .actions
            .iter()
            .filter(|x| x["action"] == "check")
            .count(),
        3
    );
    assert_eq!(calls.actions[4]["outcome"], "delivered");
    assert_eq!(calls.writes.len(), 3);
    assert_eq!(calls.writes[1]["body"]["simulated"], true);
    assert_eq!(
        calls.writes[2]["body"]["ts"],
        robot_cloud_common::parse_time(OBSERVED).unwrap()
    );
    assert!(calls.gets[1].contains("startTs="));
}

#[tokio::test]
async fn failure_retries_same_record_and_recovers_existing_device() {
    let calls = Arc::new(Mutex::new(Calls {
        fail_write: true,
        ..Default::default()
    }));
    let (_source, _platform, worker) = setup(calls.clone()).await;
    let error = format!("{:#}", worker.step().await.unwrap_err());
    assert!(error.contains("platform_sample"));
    assert!(error.contains("http_status=503"));
    assert!(!error.contains(&"s".repeat(32)));
    assert!(!error.contains(&"t".repeat(32)));
    assert!(!error.contains("sourceReport"));
    {
        let mut state = calls.lock().unwrap();
        assert_eq!(state.actions.last().unwrap()["outcome"], "retry");
        state.claimed = false;
        state.fail_write = false;
    }
    assert!(worker.step().await.unwrap());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.actions.last().unwrap()["outcome"], "delivered");
    assert_eq!(
        calls
            .writes
            .iter()
            .filter(|x| x["path"] == "/api/device")
            .count(),
        1
    );
    let payloads: Vec<_> = calls
        .writes
        .iter()
        .filter(|x| x["path"].as_str().unwrap().ends_with("/timeseries/ANY"))
        .map(|x| &x["body"])
        .collect();
    assert_eq!(payloads[0], payloads[1]);
}

#[tokio::test]
async fn conflicting_timestamp_does_not_overwrite_existing_sample() {
    let calls = Arc::new(Mutex::new(Calls {
        conflict: true,
        existing: true,
        ..Default::default()
    }));
    let (_source, _platform, worker) = setup(calls.clone()).await;
    assert!(worker.step().await.unwrap());
    let calls = calls.lock().unwrap();
    assert!(calls.writes.is_empty());
    assert_eq!(calls.actions.last().unwrap()["outcome"], "conflict");
}

#[tokio::test]
async fn same_sample_can_be_retried_after_lost_confirmation() {
    let calls = Arc::new(Mutex::new(Calls {
        same_sample: true,
        existing: true,
        ..Default::default()
    }));
    let (_source, _platform, worker) = setup(calls.clone()).await;
    assert!(worker.step().await.unwrap());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.writes.len(), 2);
    assert_eq!(calls.actions.last().unwrap()["outcome"], "delivered");
}

#[tokio::test]
async fn revoked_claim_is_never_sent_to_platform() {
    let calls = Arc::new(Mutex::new(Calls {
        revoked: true,
        ..Default::default()
    }));
    let (_source, _platform, worker) = setup(calls.clone()).await;
    assert!(worker.step().await.unwrap());
    let calls = calls.lock().unwrap();
    assert!(calls.writes.is_empty());
    assert!(calls.gets.is_empty());
    assert!(calls.actions.iter().all(|x| x["action"] != "finish"));
}
