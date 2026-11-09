use robot_cloud_common::{
    Control, Result, State, Status,
    status::{KINDS, OWNERS, TRANSITIONS},
};
use std::time::Duration;
use tonic::{Request, client::Grpc, transport::Endpoint};

// Read-only wire projection of brainstem_api/cms.proto; unknown fields are ignored.
#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
struct WireState {
    #[prost(int32, tag = "1")]
    kind: i32,
    #[prost(int32, tag = "2")]
    transition: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct WireControlStatus {
    #[prost(message, optional, tag = "1")]
    fsm: Option<WireState>,
    #[prost(bool, tag = "2")]
    motor_output_enabled: bool,
    #[prost(int32, tag = "3")]
    owner: i32,
    #[prost(bool, tag = "4")]
    critical_motor_fault: bool,
}

fn unavailable() -> std::io::Error {
    std::io::Error::other("upstream_unavailable")
}

fn enum_name(value: i32, names: &[&str]) -> Result<String> {
    usize::try_from(value)
        .ok()
        .and_then(|index| names.get(index))
        .map(|name| (*name).to_owned())
        .ok_or_else(|| unavailable().into())
}

fn normalize(value: WireControlStatus) -> Result<Status> {
    let fsm = value.fsm.ok_or_else(unavailable)?;
    Ok(Status {
        state: State {
            kind: enum_name(fsm.kind, KINDS)?,
            transition: enum_name(fsm.transition, TRANSITIONS)?,
        },
        control: Control {
            owner: enum_name(value.owner, OWNERS)?,
            motor_output_enabled: value.motor_output_enabled,
            critical_motor_fault: value.critical_motor_fault,
        },
        transport: "local-insecure".to_owned(),
        location: None,
    })
}

pub async fn read_status() -> Result<Status> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let channel = Endpoint::from_static("http://127.0.0.1:13146")
            .connect_timeout(Duration::from_secs(3))
            .connect()
            .await?;
        let mut grpc = Grpc::new(channel);
        grpc.ready().await.map_err(|_| unavailable())?;
        let mut request = Request::new(Empty {});
        request.set_timeout(Duration::from_secs(3));
        let response: tonic::Response<WireControlStatus> = grpc
            .unary(
                request,
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/brainstem.api.v1.RobotControl/GetControlStatus",
                ),
                tonic_prost::ProstCodec::default(),
            )
            .await?;
        let mut status = normalize(response.into_inner())?;
        status.location = crate::gnss::read().await;
        Ok(status)
    })
    .await
    .map_err(|_| unavailable())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn actual_wire_tags_decode_without_inventing_other_fields() {
        // fsm=GROUNDED, owner=YUNZHUO, motor/fault=true; ignored lease field=5.
        let bytes = [
            0x0a, 0x02, 0x08, 0x01, 0x10, 0x01, 0x18, 0x02, 0x20, 0x01, 0x28, 0x01,
        ];
        let status = normalize(WireControlStatus::decode(bytes.as_slice()).unwrap()).unwrap();
        assert_eq!(status.state.kind, "CMS_STATE_KIND_GROUNDED");
        assert_eq!(status.state.transition, "CMS_TRANSITION_KIND_NONE");
        assert_eq!(status.control.owner, "CONTROL_OWNER_YUNZHUO");
        assert!(status.control.motor_output_enabled);
        assert!(status.control.critical_motor_fault);
        assert_eq!(status.transport, "local-insecure");
    }

    #[test]
    fn missing_state_and_unknown_enums_are_rejected() {
        assert!(normalize(WireControlStatus::default()).is_err());
        for (kind, transition, owner) in [(99, 0, 0), (1, -1, 0), (1, 0, 99)] {
            assert!(
                normalize(WireControlStatus {
                    fsm: Some(WireState { kind, transition }),
                    owner,
                    ..Default::default()
                })
                .is_err()
            );
        }
    }
}
