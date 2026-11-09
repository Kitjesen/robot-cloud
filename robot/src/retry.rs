use rumqttc::v5::{ConnectionError, TlsError, mqttbytes::v5::ConnectReturnCode};
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub(super) enum Exit {
    Stopped,
    Retry(&'static str),
    Config(&'static str),
}

pub(super) struct Backoff {
    next_seconds: u64,
}

impl Backoff {
    pub(super) fn new() -> Self {
        Self { next_seconds: 2 }
    }
    pub(super) fn next(&mut self, online_for: Duration, jitter_ms: u64) -> Duration {
        if online_for >= Duration::from_secs(60) {
            self.next_seconds = 2;
        }
        let delay =
            Duration::from_millis((self.next_seconds * 1000 + jitter_ms.min(1000)).min(30000));
        self.next_seconds = (self.next_seconds * 2).min(30);
        delay
    }
}

pub(super) fn refused(code: &ConnectReturnCode) -> Exit {
    match code {
        ConnectReturnCode::NotAuthorized
        | ConnectReturnCode::BadUserNamePassword
        | ConnectReturnCode::Banned
        | ConnectReturnCode::BadAuthenticationMethod => Exit::Config("auth_rejected"),
        _ => Exit::Retry("broker_unavailable"),
    }
}

pub(super) fn classify(error: &ConnectionError) -> Exit {
    match error {
        ConnectionError::ConnectionRefused(code) => refused(code),
        ConnectionError::Tls(
            TlsError::NoValidCertInChain
            | TlsError::NoValidClientCertInChain
            | TlsError::NoValidKeyInChain
            | TlsError::DNSName(_),
        ) => Exit::Config("local_tls_config_error"),
        ConnectionError::Timeout(_) => Exit::Retry("connect_timeout"),
        ConnectionError::Tls(_) => Exit::Retry("tls_failed"),
        ConnectionError::Io(_) => Exit::Retry("network_unavailable"),
        _ => Exit::Retry("mqtt_connection_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_reset() {
        let mut backoff = Backoff { next_seconds: 2 };
        let seconds: Vec<_> = (0..7)
            .map(|_| backoff.next(Duration::ZERO, 0).as_secs())
            .collect();
        assert_eq!(seconds, [2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(
            backoff.next(Duration::from_secs(59), 900),
            Duration::from_secs(30)
        );
        assert_eq!(
            backoff.next(Duration::from_secs(60), 900),
            Duration::from_millis(2900)
        );
    }

    #[test]
    fn error_policy() {
        let io = ConnectionError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "test",
        ));
        assert_eq!(classify(&io), Exit::Retry("network_unavailable"));
        assert_eq!(
            refused(&ConnectReturnCode::ServerBusy),
            Exit::Retry("broker_unavailable")
        );
        assert_eq!(
            refused(&ConnectReturnCode::NotAuthorized),
            Exit::Config("auth_rejected")
        );
        assert_eq!(
            classify(&ConnectionError::Tls(TlsError::NoValidKeyInChain)),
            Exit::Config("local_tls_config_error")
        );
    }
}
