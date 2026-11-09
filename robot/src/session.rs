use crate::{
    choice::Choice,
    device::Device,
    grpc,
    retry::{self, Exit},
    source::Source,
};
use robot_cloud_common::{
    ReadRequest, Result, Status, connect_mqtt, now_ms,
    reporting::{HELLO_MS, Permission, REPORT_MS},
    shutdown_signal,
};
use rumqttc::{
    Outgoing,
    v5::{
        AsyncClient, Event,
        mqttbytes::{
            QoS,
            v5::{ConnectReturnCode, Filter, Packet, SubscribeReasonCode},
        },
    },
};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::time::{MissedTickBehavior, interval};

type ReadFuture = Pin<Box<dyn Future<Output = (Result<Status>, i64)> + Send>>;

struct Read {
    future: ReadFuture,
    request: Option<ReadRequest>,
    permission: Permission,
    report: bool,
}

impl Read {
    fn start(request: Option<ReadRequest>, permission: Permission, report: bool) -> Self {
        Self {
            future: Box::pin(async {
                let status = grpc::read_status().await;
                (status, now_ms())
            }),
            request,
            permission,
            report,
        }
    }
}

#[derive(Default)]
struct Publications {
    queued: VecDeque<bool>,
    report_id: Option<u16>,
}

impl Publications {
    fn busy(&self) -> bool {
        self.report_id.is_some() || self.queued.iter().any(|report| *report)
    }

    fn publish(&mut self, client: &AsyncClient, topic: &str, bytes: Vec<u8>, report: bool) -> bool {
        if client
            .try_publish(topic, QoS::AtLeastOnce, false, bytes)
            .is_err()
        {
            return false;
        }
        self.queued.push_back(report);
        true
    }

    fn sent(&mut self, id: u16) {
        if self.queued.pop_front() == Some(true) {
            self.report_id = Some(id);
        }
    }

    fn acknowledged(&mut self, id: u16) {
        if self.report_id == Some(id) {
            self.report_id = None;
        }
    }
}

pub(super) async fn run(
    device: &mut Device,
    choice: &Choice,
    source: &mut Source,
) -> Result<(Exit, Duration)> {
    let (client, mut mqtt) = connect_mqtt(&device.id)?;
    mqtt.options.set_connection_timeout(5);
    let mut online: Option<Instant> = None;
    let mut subscription = None;
    let mut read: Option<Read> = None;
    let mut publications = Publications::default();
    let mut last_read: Option<Instant> = None;
    let mut hello = interval(Duration::from_millis(HELLO_MS));
    let mut sample = interval(Duration::from_millis(REPORT_MS));
    hello.set_missed_tick_behavior(MissedTickBehavior::Skip);
    sample.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let stop = shutdown_signal();
    tokio::pin!(stop);
    let exit = loop {
        let now = now_ms();
        device.reporting.expire(now, Instant::now());
        if read
            .as_ref()
            .is_some_and(|work| !device.reporting.is_current(&work.permission))
        {
            read = None;
        }
        let deadline = device.reporting.deadline();
        let video_deadline = source.deadline();
        tokio::select! {
            biased;
            _ = &mut stop => break Exit::Stopped,
            _ = choice.closed() => break Exit::Stopped,
            _ = async { tokio::time::sleep_until(video_deadline.expect("video lease").into()).await }, if video_deadline.is_some() => {
                if let Err(error) = source.request_stop() { eprintln!("video_stop_error reason={error}"); }
            },
            _ = async { tokio::time::sleep_until(deadline.expect("active lease").into()).await }, if deadline.is_some() => {
                device.reporting.expire(now_ms(), Instant::now());
                read = None;
            },
            event = mqtt.poll() => match event {
                Ok(Event::Incoming(Packet::ConnAck(ack))) => {
                    if ack.code != ConnectReturnCode::Success { break retry::refused(&ack.code); }
                    online = None;
                    read = None;
                    device.reporting.connect(now_ms(), Instant::now());
                    source.connected();
                    if client.try_subscribe_many([
                        Filter::new(&device.topics.requests, QoS::AtLeastOnce),
                        Filter::new(&device.topics.control, QoS::AtLeastOnce),
                        Filter::new(&device.topics.video, QoS::AtLeastOnce),
                    ]).is_err() {
                        break Exit::Retry("subscribe_queue_unavailable");
                    }
                }
                Ok(Event::Outgoing(Outgoing::Subscribe(id))) => subscription = Some(id),
                Ok(Event::Incoming(Packet::SubAck(ack))) => {
                    if subscription != Some(ack.pkid) { continue; }
                    if ack.return_codes.len() != 3 || !ack.return_codes[..2].iter().all(|code| matches!(code, SubscribeReasonCode::Success(_))) {
                        break if ack.return_codes.iter().any(|code| matches!(code,
                            SubscribeReasonCode::NotAuthorized | SubscribeReasonCode::TopicFilterInvalid)) {
                            Exit::Config("subscription_rejected")
                        } else { Exit::Retry("subscription_unavailable") };
                    }
                    if !matches!(ack.return_codes[2], SubscribeReasonCode::Success(_)) {
                        eprintln!("video_source_unavailable reason=subscription_rejected");
                    }
                    subscription = None;
                    online = Some(Instant::now());
                    hello.reset_immediately();
                    println!("cloud_connected");
                }
                Ok(Event::Outgoing(Outgoing::Publish(id))) => publications.sent(id),
                Ok(Event::Incoming(Packet::PubAck(ack))) => publications.acknowledged(ack.pkid),
                Ok(Event::Incoming(Packet::Publish(message))) => {
                    if online.is_none() || message.retain { continue; }
                    let now = now_ms();
                    if message.topic.as_ref() == device.topics.video.as_bytes() {
                        if let Ok(control) = device.video_control(&message.payload, now)
                            && let Err(error) = source.apply(control).await {
                                eprintln!("video_source_error reason={error}");
                            }
                    } else if message.topic.as_ref() == device.topics.control.as_bytes() {
                        if let Ok(bytes) = device.control(&message.payload, now, Instant::now()) {
                            if device.reporting.disabled() { read = None; }
                            if !publications.publish(&client, &device.topics.acks, bytes, false) {
                                break Exit::Retry("publish_queue_unavailable");
                            }
                        }
                    } else if message.topic.as_ref() == device.topics.requests.as_bytes()
                        && read.as_ref().is_none_or(|work| work.request.is_none())
                        && let Ok(Some(request)) = device.accept(&message.payload, now)
                        && let Some(permission) = device.reporting.active().cloned() {
                            match &mut read {
                                Some(work) => work.request = Some(request),
                                None => {
                                    let report = !publications.busy()
                                        && last_read.is_none_or(|last| last.elapsed() >= Duration::from_millis(REPORT_MS));
                                    read = Some(Read::start(Some(request), permission, report));
                                    last_read = Some(Instant::now());
                                }
                            }
                    }
                }
                Ok(Event::Incoming(Packet::Disconnect(_))) => break Exit::Retry("broker_disconnected"),
                Err(error) => break retry::classify(&error),
                _ => {}
            },
            (status, observed) = async { read.as_mut().expect("active read").future.as_mut().await }, if read.is_some() => {
                let work = read.take().expect("completed read");
                device.reporting.expire(now_ms(), Instant::now());
                if !device.reporting.is_current(&work.permission) { continue; }
                if let Some(request) = work.request {
                    let reply_status = status.as_ref().cloned().map_err(|_| std::io::Error::other("upstream_unavailable").into());
                    match device.reply(request, reply_status, observed) {
                        Ok(Some(bytes)) => {
                            if !publications.publish(&client, &device.topics.replies, bytes, false) {
                                break Exit::Retry("publish_queue_unavailable");
                            }
                        }
                        Ok(None) => {}
                        Err(_) => eprintln!("cloud_reply_error"),
                    }
                }
                if work.report {
                    match device.report(&work.permission, status, observed) {
                        Ok(Some(bytes)) => {
                            if !publications.publish(&client, &device.topics.reports, bytes, true) {
                                break Exit::Retry("publish_queue_unavailable");
                            }
                        }
                        Ok(None) => {}
                        Err(_) => eprintln!("cloud_report_error"),
                    }
                }
            },
            _ = hello.tick(), if online.is_some() => {
                let bytes = device.hello(now_ms())?;
                if !publications.publish(&client, &device.topics.hello, bytes, false) {
                    break Exit::Retry("publish_queue_unavailable");
                }
            },
            _ = sample.tick(), if online.is_some() => {
                if read.is_none() && !publications.busy()
                    && last_read.is_none_or(|last| last.elapsed() >= Duration::from_millis(REPORT_MS))
                    && let Some(permission) = device.reporting.active() {
                        read = Some(Read::start(None, permission.clone(), true));
                        last_read = Some(Instant::now());
                }
            }
        }
    };
    // The session owns the read future and MQTT queue; both are dropped on exit.
    device.reporting.disconnect();
    if let Err(error) = source.stop().await {
        eprintln!("video_stop_error reason={error}");
    }
    Ok((exit, online.map_or(Duration::ZERO, |time| time.elapsed())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_matching_report_ack_releases_sampling() {
        let mut publications = Publications::default();
        publications.queued.extend([false, true, false]);
        assert!(publications.busy());
        publications.sent(10);
        publications.acknowledged(10);
        assert!(publications.busy());
        publications.sent(11);
        publications.sent(12);
        publications.acknowledged(12);
        assert!(publications.busy());
        publications.acknowledged(11);
        assert!(!publications.busy());
    }
}
