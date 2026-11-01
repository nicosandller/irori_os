//! What a camera notices: motion, and on cameras that tell them apart, people, vehicles and
//! animals. Read over ONVIF's PullPoint events.
//!
//! A subscription is made with `CreatePullPointSubscription`, then asked for what happened
//! with a long-polling `PullMessages`, and kept alive with `Renew`. Its lifetime is short, so
//! a subscription left behind by a restart runs out on its own instead of taking one of the
//! few a camera has.
//!
//! Topics differ by vendor, so they're sorted by what they name rather than matched exactly:
//! Reolink says `RuleEngine/MyRuleDetector/PeopleDetect`, Tapo says
//! `RuleEngine/CellMotionDetector/Motion`, others `VideoSource/MotionAlarm`.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::http::{HttpError, Target};
use crate::soap::{self, Soap, find, find_all, text};

/// How many pulls in a row may go wrong before a subscription is given up.
const PULL_TRIES: usize = 3;
const EVENTS: &str = "http://www.onvif.org/ver10/events/wsdl";
/// How long a subscription lasts unless renewed.
const LIFETIME: Duration = Duration::from_secs(120);
/// How often it's renewed.
const RENEW: Duration = Duration::from_secs(60);
/// How long one `PullMessages` waits for something to happen. Under the HTTP timeout.
const PULL: Duration = Duration::from_secs(5);
/// Waits between subscription attempts, growing to the last.
const BACKOFF: [u64; 5] = [5, 15, 60, 180, 300];

/// What a camera can say it noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Detects {
    Motion,
    Person,
    Vehicle,
    Animal,
}

impl Detects {
    /// The handle part, as in `motion:<camera id>`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Motion => "motion",
            Self::Person => "person",
            Self::Vehicle => "vehicle",
            Self::Animal => "animal",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Motion => "Motion",
            Self::Person => "Person",
            Self::Vehicle => "Vehicle",
            Self::Animal => "Animal",
        }
    }
}

/// What a topic (and the name of the value in it) is about.
pub fn classify(topic: &str, item: &str) -> Option<Detects> {
    let said = format!("{topic}/{item}").to_ascii_lowercase();
    if said.contains("people") || said.contains("person") || said.contains("human") {
        Some(Detects::Person)
    } else if said.contains("vehicle") {
        Some(Detects::Vehicle)
    } else if said.contains("dogcat") || said.contains("animal") || said.contains("pet") {
        Some(Detects::Animal)
    } else if said.contains("motion") {
        Some(Detects::Motion)
    } else {
        None
    }
}

/// What the camera says it can tell, from its `GetEventProperties` answer: each topic it offers,
/// sorted. Always includes motion if it offers any topic about it.
pub fn offered(answer: &str) -> Vec<Detects> {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Vec::new();
    };
    let Some(topics) = find(doc.root(), "TopicSet") else {
        return Vec::new();
    };
    let mut found: Vec<Detects> = Vec::new();
    for node in topics.descendants().filter(roxmltree::Node::is_element) {
        let is_topic = node
            .attributes()
            .any(|attribute| attribute.name() == "topic" && attribute.value() == "true");
        if !is_topic {
            continue;
        }
        // The topic's path: its own name and its parents' up to the set.
        let path: Vec<&str> = node
            .ancestors()
            .take_while(|ancestor| *ancestor != topics)
            .filter(roxmltree::Node::is_element)
            .map(|ancestor| ancestor.tag_name().name())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // What the values in it are called, for topics named only by them (`IsPeople`).
        let items: Vec<String> = find_all(node, "SimpleItemDescription")
            .filter_map(|item| soap::attribute(item, "Name"))
            .collect();
        let topic = path.join("/");
        let kinds = if items.is_empty() {
            classify(&topic, "").into_iter().collect::<Vec<_>>()
        } else {
            items
                .iter()
                .filter_map(|item| classify(&topic, item))
                .collect()
        };
        for kind in kinds {
            if !found.contains(&kind) {
                found.push(kind);
            }
        }
    }
    found.sort();
    found
}

/// A sensor turning on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub detects: Detects,
    pub on: bool,
}

/// The changes in a `PullMessages` answer, in order.
pub fn changes(answer: &str) -> Vec<Change> {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for message in find_all(doc.root(), "NotificationMessage") {
        let Some(topic) = text(message, &["Topic"]) else {
            continue;
        };
        // Only the data: the source items name the video source or rule, not what happened.
        let Some(data) = find(message, "Data") else {
            continue;
        };
        for item in find_all(data, "SimpleItem") {
            let name = soap::attribute(item, "Name").unwrap_or_default();
            let Some(on) = soap::attribute(item, "Value").and_then(|value| truth(&value)) else {
                continue;
            };
            if let Some(detects) = classify(&topic, &name) {
                out.push(Change { detects, on });
            }
        }
    }
    out
}

fn truth(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" => Some(true),
        "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// Asks the camera what it can tell. Only a refused login is an error: a camera without
/// events just has no sensors.
pub async fn properties(soap: &Soap, events: &Target) -> Result<Vec<Detects>, HttpError> {
    match soap
        .call(
            events,
            &format!("{EVENTS}/EventPortType/GetEventPropertiesRequest"),
            "<tev:GetEventProperties/>",
        )
        .await
    {
        Ok(answer) => Ok(offered(&answer)),
        Err(HttpError::Refused) => Err(HttpError::Refused),
        Err(_) => Ok(Vec::new()),
    }
}

/// Follows the camera's events until `changes` closes, sending each change. Returns only when
/// there is nothing more to do: `Ok` when nobody listens any more, `Err(Refused)` when the
/// camera turned the login down, which must not be tried again.
pub async fn follow(
    soap: &Soap,
    events: &Target,
    detects: &[Detects],
    changes: &mpsc::Sender<Change>,
) -> Result<(), HttpError> {
    let mut started = false;
    let mut attempt = 0usize;
    loop {
        if changes.is_closed() {
            return Ok(());
        }
        match subscribe(soap, events).await {
            Ok(subscription) => {
                attempt = 0;
                match pull(soap, &subscription, detects, &mut started, changes).await {
                    Err(HttpError::Refused) => return Err(HttpError::Refused),
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        // Cameras hold only a few subscriptions (a Tapo, two or three): give
                        // this one back before asking for another.
                        unsubscribe(soap, &subscription).await;
                        tracing::info!(ip = %soap.camera.ip, "a camera's events stopped: {error}")
                    }
                }
            }
            Err(HttpError::Refused) => return Err(HttpError::Refused),
            Err(error) => {
                tracing::info!(ip = %soap.camera.ip, "a camera's events couldn't be followed: {error}")
            }
        }
        let wait = BACKOFF[attempt.min(BACKOFF.len() - 1)];
        attempt += 1;
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

/// Where a subscription is: the address to send to, and the full address to name in `To`.
struct Subscription {
    target: Target,
    address: String,
}

async fn subscribe(soap: &Soap, events: &Target) -> Result<Subscription, HttpError> {
    let body = format!(
        "<tev:CreatePullPointSubscription><tev:InitialTerminationTime>{}\
         </tev:InitialTerminationTime></tev:CreatePullPointSubscription>",
        soap::duration(LIFETIME)
    );
    let answer = soap
        .call(
            events,
            &format!("{EVENTS}/EventPortType/CreatePullPointSubscriptionRequest"),
            &body,
        )
        .await?;
    let doc = roxmltree::Document::parse(&answer)
        .map_err(|_| HttpError::Failed("the camera's subscription can't be read".into()))?;
    let address = find(doc.root(), "SubscriptionReference")
        .and_then(|reference| text(reference, &["Address"]))
        .ok_or_else(|| HttpError::Failed("the camera gave no subscription".into()))?;
    let target = Target::parse(&address).map_err(HttpError::Failed)?;
    Ok(Subscription { target, address })
}

/// Pulls until something goes wrong or nobody listens.
/// `started` is whether the first answer has come yet: after it, whatever the camera didn't say
/// is taken to be off, since some cameras (a Tapo) only ever send changes.
async fn pull(
    soap: &Soap,
    subscription: &Subscription,
    detects: &[Detects],
    started: &mut bool,
    changes: &mpsc::Sender<Change>,
) -> Result<(), HttpError> {
    let pull_action = format!("{EVENTS}/PullPointSubscription/PullMessagesRequest");
    let pull_body = format!(
        "<tev:PullMessages><tev:Timeout>{}</tev:Timeout>\
         <tev:MessageLimit>32</tev:MessageLimit></tev:PullMessages>",
        soap::duration(PULL)
    );
    let mut renew_at = tokio::time::Instant::now() + RENEW;
    let mut failures = 0usize;
    loop {
        if changes.is_closed() {
            unsubscribe(soap, subscription).await;
            return Ok(());
        }
        if tokio::time::Instant::now() >= renew_at {
            let body = format!(
                "<wsnt:Renew><wsnt:TerminationTime>{}</wsnt:TerminationTime></wsnt:Renew>",
                soap::duration(LIFETIME)
            );
            soap.call_to(
                &subscription.target,
                "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/RenewRequest",
                &body,
                Some(&subscription.address),
            )
            .await
            .map_err(|error| match error {
                HttpError::Failed(why) => HttpError::Failed(format!("renewing: {why}")),
                HttpError::Unreachable(why) => HttpError::Unreachable(format!("renewing: {why}")),
                HttpError::Refused => HttpError::Refused,
            })?;
            renew_at = tokio::time::Instant::now() + RENEW;
        }
        // A pull that goes wrong once is asked again on the same subscription: only a few in a
        // row end it.
        let answer = match soap
            .call_to(
                &subscription.target,
                &pull_action,
                &pull_body,
                Some(&subscription.address),
            )
            .await
        {
            Ok(answer) => {
                failures = 0;
                answer
            }
            Err(HttpError::Refused) => return Err(HttpError::Refused),
            Err(error) if failures + 1 < PULL_TRIES => {
                failures += 1;
                tracing::debug!("a camera's events didn't come this time: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let said = self::changes(&answer);
        let unsaid: Vec<Change> = if *started {
            Vec::new()
        } else {
            *started = true;
            detects
                .iter()
                .filter(|detects| !said.iter().any(|change| change.detects == **detects))
                .map(|detects| Change {
                    detects: *detects,
                    on: false,
                })
                .collect()
        };
        for change in unsaid.into_iter().chain(said) {
            if changes.send(change).await.is_err() {
                return Ok(());
            }
        }
    }
}

async fn unsubscribe(soap: &Soap, subscription: &Subscription) {
    let _ = soap
        .call_to(
            &subscription.target,
            "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest",
            "<wsnt:Unsubscribe/>",
            Some(&subscription.address),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_sorted_by_what_they_name() {
        let cases = [
            (
                "tns1:RuleEngine/CellMotionDetector/Motion",
                "IsMotion",
                Some(Detects::Motion),
            ),
            (
                "tns1:VideoSource/MotionAlarm",
                "State",
                Some(Detects::Motion),
            ),
            (
                "tns1:RuleEngine/MyRuleDetector/PeopleDetect",
                "State",
                Some(Detects::Person),
            ),
            (
                "tns1:RuleEngine/MyRuleDetector/VehicleDetect",
                "State",
                Some(Detects::Vehicle),
            ),
            (
                "tns1:RuleEngine/MyRuleDetector/DogCatDetect",
                "State",
                Some(Detects::Animal),
            ),
            (
                "tns1:RuleEngine/TPSmartEventDetector/TPSmartEvent",
                "IsPeople",
                Some(Detects::Person),
            ),
            ("tns1:VideoSource/ImageTooDark", "State", None),
        ];
        for (topic, item, expected) in cases {
            assert_eq!(classify(topic, item), expected, "{topic} {item}");
        }
    }

    #[test]
    fn a_pull_answer_turns_sensors_on_and_off() {
        let answer = r#"<Envelope><Body><PullMessagesResponse>
          <CurrentTime>2026-11-01T10:00:00Z</CurrentTime>
          <NotificationMessage><Topic Dialect="x">tns1:RuleEngine/CellMotionDetector/Motion</Topic>
            <Message><Message UtcTime="2026-11-01T10:00:00Z" PropertyOperation="Changed">
              <Source><SimpleItem Name="VideoSourceConfigurationToken" Value="1"/></Source>
              <Data><SimpleItem Name="IsMotion" Value="true"/></Data>
            </Message></Message></NotificationMessage>
          <NotificationMessage><Topic>tns1:RuleEngine/MyRuleDetector/PeopleDetect</Topic>
            <Message><Message><Data><SimpleItem Name="State" Value="false"/></Data></Message></Message>
          </NotificationMessage>
        </PullMessagesResponse></Body></Envelope>"#;
        assert_eq!(
            changes(answer),
            [
                Change {
                    detects: Detects::Motion,
                    on: true
                },
                Change {
                    detects: Detects::Person,
                    on: false
                },
            ]
        );
    }

    #[test]
    fn the_offered_topics_say_which_sensors_there_are() {
        let answer = r#"<Envelope><Body><GetEventPropertiesResponse>
          <TopicSet>
            <tns1:RuleEngine xmlns:tns1="http://www.onvif.org/ver10/topics" xmlns:wstop="http://docs.oasis-open.org/wsn/t-1">
              <MyRuleDetector>
                <PeopleDetect wstop:topic="true"><MessageDescription><Data>
                  <SimpleItemDescription Name="State" Type="xs:boolean"/></Data></MessageDescription></PeopleDetect>
                <FaceDetect wstop:topic="true"/>
              </MyRuleDetector>
              <CellMotionDetector><Motion wstop:topic="true"><MessageDescription><Data>
                <SimpleItemDescription Name="IsMotion" Type="xs:boolean"/></Data></MessageDescription></Motion>
              </CellMotionDetector>
            </tns1:RuleEngine>
          </TopicSet>
        </GetEventPropertiesResponse></Body></Envelope>"#;
        assert_eq!(offered(answer), [Detects::Motion, Detects::Person]);
    }
}
