//! Turning a camera: ONVIF PTZ.
//!
//! A move is a `ContinuousMove` at a speed, always followed by a `Stop` once its time is up, so
//! a page that loses its connection mid-press never leaves a camera spinning. Moves for one
//! camera go through one worker ([`run`]), one at a time; a new move before the old one's time
//! is up just moves the stop later, which is what holding a button does.

use std::time::Duration;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::{CameraMove, CameraPreset, Service};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::http::{HttpError, Target};
use crate::soap::{self, Soap, attribute, find, find_all, text};

const PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";
/// The most presets kept: what an entity's state can carry.
const MAX_PRESETS: usize = 256;

/// What the camera can do, from its PTZ node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Abilities {
    pub pan_tilt: bool,
    pub zoom: bool,
    pub presets: bool,
}

/// One camera's PTZ service, for the profile that turns.
#[derive(Debug, Clone)]
pub struct Ptz {
    pub soap: Soap,
    pub service: Target,
    pub token: String,
}

/// What a worker reports back.
#[derive(Debug)]
pub enum Report {
    Presets(Vec<CameraPreset>),
    /// The camera refused the login: stop using it.
    Refused,
}

/// Reads what a camera's PTZ can do from its `GetNodes` answer.
pub fn abilities(answer: &str) -> Abilities {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Abilities::default();
    };
    let Some(spaces) = find(doc.root(), "SupportedPTZSpaces") else {
        return Abilities::default();
    };
    let has = |name: &str| find(spaces, name).is_some();
    let presets = find(doc.root(), "MaximumNumberOfPresets")
        .and_then(|node| node.text())
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|count| count > 0);
    Abilities {
        pan_tilt: has("ContinuousPanTiltVelocitySpace"),
        zoom: has("ContinuousZoomVelocitySpace"),
        presets,
    }
}

/// Reads presets from a `GetPresets` answer, skipping any without a token and any empty slot.
///
/// A Tapo lists every slot whether or not anything was saved in it: an empty one keeps its
/// default name (`Preset3`) and the same position as the other empty ones, and going to it
/// fails. So a preset with its default name, at the same position as another with its default
/// name, is left out.
pub fn presets(answer: &str) -> Vec<CameraPreset> {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Vec::new();
    };
    let unnamed_at = |preset: roxmltree::Node| -> Option<String> {
        let id = attribute(preset, "token")?;
        let name = text(preset, &["Name"]).unwrap_or_default();
        (name.is_empty() || name == format!("Preset{id}")).then(|| position(preset))
    };
    let unnamed: Vec<String> = find_all(doc.root(), "Preset")
        .filter_map(unnamed_at)
        .collect();
    let mut out: Vec<CameraPreset> = Vec::new();
    for preset in find_all(doc.root(), "Preset") {
        let Some(id) = attribute(preset, "token").filter(|id| fits(id)) else {
            continue;
        };
        if out.iter().any(|known| known.id == id) {
            continue;
        }
        if let Some(at) = unnamed_at(preset)
            && unnamed.iter().filter(|other| **other == at).count() > 1
        {
            continue;
        }
        let name = text(preset, &["Name"])
            .filter(|name| fits(name))
            .unwrap_or_else(|| id.clone());
        out.push(CameraPreset { id, name });
        if out.len() == MAX_PRESETS {
            break;
        }
    }
    out
}

/// Where a preset points, as text to compare.
fn position(preset: roxmltree::Node) -> String {
    let Some(at) = find(preset, "PTZPosition") else {
        return String::new();
    };
    let pan_tilt = find(at, "PanTilt");
    let zoom = find(at, "Zoom");
    let get = |node: Option<roxmltree::Node>, name: &str| {
        node.and_then(|node| attribute(node, name))
            .unwrap_or_default()
    };
    format!(
        "{} {} {}",
        get(pan_tilt, "x"),
        get(pan_tilt, "y"),
        get(zoom, "x")
    )
}

fn fits(text: &str) -> bool {
    !text.is_empty() && text.len() <= 255 && !text.chars().any(char::is_control)
}

/// Asks the camera what its PTZ can do. Only a refused login is an error.
pub async fn ask_abilities(soap: &Soap, service: &Target) -> Result<Abilities, HttpError> {
    match soap
        .call(service, &format!("{PTZ}/GetNodes"), "<tptz:GetNodes/>")
        .await
    {
        Ok(answer) => Ok(abilities(&answer)),
        Err(HttpError::Refused) => Err(HttpError::Refused),
        Err(_) => Ok(Abilities::default()),
    }
}

impl Ptz {
    pub async fn presets(&self) -> Result<Vec<CameraPreset>, HttpError> {
        let body = format!(
            "<tptz:GetPresets><tptz:ProfileToken>{}</tptz:ProfileToken></tptz:GetPresets>",
            soap::escape(&self.token)
        );
        let answer = self
            .soap
            .call(&self.service, &format!("{PTZ}/GetPresets"), &body)
            .await?;
        Ok(presets(&answer))
    }

    async fn start(&self, moving: &CameraMove) -> Result<(), HttpError> {
        let mut velocity = String::new();
        if moving.pan.is_some() || moving.tilt.is_some() {
            velocity.push_str(&format!(
                "<tt:PanTilt x=\"{}\" y=\"{}\"/>",
                speed(moving.pan),
                speed(moving.tilt)
            ));
        }
        if moving.zoom.is_some() {
            velocity.push_str(&format!("<tt:Zoom x=\"{}\"/>", speed(moving.zoom)));
        }
        let body = format!(
            "<tptz:ContinuousMove><tptz:ProfileToken>{}</tptz:ProfileToken>\
             <tptz:Velocity>{velocity}</tptz:Velocity></tptz:ContinuousMove>",
            soap::escape(&self.token)
        );
        self.soap
            .call(&self.service, &format!("{PTZ}/ContinuousMove"), &body)
            .await
            .map(|_| ())
    }

    async fn stop(&self) -> Result<(), HttpError> {
        let body = format!(
            "<tptz:Stop><tptz:ProfileToken>{}</tptz:ProfileToken>\
             <tptz:PanTilt>true</tptz:PanTilt><tptz:Zoom>true</tptz:Zoom></tptz:Stop>",
            soap::escape(&self.token)
        );
        self.soap
            .call(&self.service, &format!("{PTZ}/Stop"), &body)
            .await
            .map(|_| ())
    }

    async fn go_to(&self, preset: &str) -> Result<(), HttpError> {
        let body = format!(
            "<tptz:GotoPreset><tptz:ProfileToken>{}</tptz:ProfileToken>\
             <tptz:PresetToken>{}</tptz:PresetToken></tptz:GotoPreset>",
            soap::escape(&self.token),
            soap::escape(preset)
        );
        self.soap
            .call(&self.service, &format!("{PTZ}/GotoPreset"), &body)
            .await
            .map(|_| ())
    }
}

/// A speed for ONVIF: -1 to 1, with a dot whatever the locale.
fn speed(value: Option<f64>) -> String {
    format!("{:.3}", value.unwrap_or(0.0).clamp(-1.0, 1.0))
}

/// Runs one camera's moves until `calls` closes. Reads its presets first, then again after a
/// move to one and every few minutes, and sends them when they change.
pub async fn run(
    ptz: Ptz,
    with_presets: bool,
    mut calls: mpsc::Receiver<IncomingCall>,
    reports: mpsc::Sender<Report>,
) {
    const PRESETS_EVERY: Duration = Duration::from_secs(10 * 60);
    let mut stop_at: Option<Instant> = None;
    let mut presets_at = Instant::now();
    let mut known: Option<Vec<CameraPreset>> = None;
    loop {
        if with_presets && Instant::now() >= presets_at {
            presets_at = Instant::now() + PRESETS_EVERY;
            match ptz.presets().await {
                Ok(now) if known.as_ref() != Some(&now) => {
                    known = Some(now.clone());
                    if reports.send(Report::Presets(now)).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(HttpError::Refused) => {
                    let _ = reports.send(Report::Refused).await;
                    return;
                }
                Err(error) => tracing::debug!("a camera's presets couldn't be read: {error}"),
            }
        }
        let call = tokio::select! {
            call = calls.recv() => call,
            () = sleep_until(stop_at) => {
                stop_at = None;
                if let Err(HttpError::Refused) = ptz.stop().await {
                    let _ = reports.send(Report::Refused).await;
                    return;
                }
                continue;
            }
            () = tokio::time::sleep_until(presets_at), if with_presets => continue,
        };
        let Some(call) = call else {
            // Nothing more will come: make sure it isn't left moving.
            if stop_at.is_some() {
                let _ = ptz.stop().await;
            }
            return;
        };
        let result = match &call.call.service {
            Service::CameraMove(moving) => {
                let result = ptz.start(moving).await;
                if result.is_ok() {
                    stop_at = Some(Instant::now() + moving.duration());
                }
                result
            }
            Service::CameraStop => {
                stop_at = None;
                ptz.stop().await
            }
            Service::CameraGoToPreset(preset) => {
                stop_at = None;
                let result = ptz.go_to(&preset.preset).await;
                // It may have been a preset saved since they were last read.
                presets_at = Instant::now();
                result
            }
            _ => {
                call.reply(Err(ServiceError::failed("this camera can't do that")));
                continue;
            }
        };
        match result {
            Ok(()) => call.reply(Ok(())),
            Err(HttpError::Refused) => {
                call.reply(Err(ServiceError::unavailable(
                    "the camera refused its login",
                )));
                let _ = reports.send(Report::Refused).await;
                return;
            }
            Err(error) => call.reply(Err(ServiceError::failed(error.to_string()))),
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_says_how_the_camera_turns() {
        let answer = r#"<Envelope><Body><GetNodesResponse><PTZNode token="node">
          <SupportedPTZSpaces>
            <ContinuousPanTiltVelocitySpace><URI>x</URI></ContinuousPanTiltVelocitySpace>
            <RelativePanTiltTranslationSpace><URI>y</URI></RelativePanTiltTranslationSpace>
          </SupportedPTZSpaces>
          <MaximumNumberOfPresets>8</MaximumNumberOfPresets>
        </PTZNode></GetNodesResponse></Body></Envelope>"#;
        assert_eq!(
            abilities(answer),
            Abilities {
                pan_tilt: true,
                zoom: false,
                presets: true
            }
        );
        assert_eq!(abilities("<Envelope/>"), Abilities::default());
    }

    #[test]
    fn presets_keep_their_tokens_and_names() {
        let answer = r#"<Envelope><Body><GetPresetsResponse>
          <Preset token="1"><Name>Driveway</Name></Preset>
          <Preset token="2"/>
          <Preset><Name>no token</Name></Preset>
          <Preset token="1"><Name>again</Name></Preset>
        </GetPresetsResponse></Body></Envelope>"#;
        assert_eq!(
            presets(answer),
            [
                CameraPreset {
                    id: "1".into(),
                    name: "Driveway".into()
                },
                CameraPreset {
                    id: "2".into(),
                    name: "2".into()
                },
            ]
        );
    }

    #[test]
    fn a_tapos_empty_slots_are_not_presets() {
        let slot = |n: u32| {
            format!(
                "<Preset token=\"{n}\"><Name>Preset{n}</Name><PTZPosition>\
                 <PanTilt space=\"s\" y=\"0\" x=\"-1\"></PanTilt></PTZPosition>\
                 </Preset>"
            )
        };
        let empty: String = (1..=8).map(slot).collect();
        assert!(roxmltree::Document::parse(&format!("<a>{empty}</a>")).is_ok());
        let answer = format!(
            "<Envelope><Body><GetPresetsResponse>{empty}</GetPresetsResponse></Body></Envelope>"
        );
        assert_eq!(presets(&answer), []);

        // One saved somewhere else, still with its default name, is kept.
        let saved = "<Preset token=\"9\"><Name>Preset9</Name><PTZPosition>\
                     <PanTilt space=\"s\" y=\"0.2\" x=\"0.4\"></PanTilt></PTZPosition>\
                     </Preset>";
        let answer = format!(
            "<Envelope><Body><GetPresetsResponse>{empty}{saved}</GetPresetsResponse></Body></Envelope>"
        );
        assert_eq!(
            presets(&answer),
            [CameraPreset {
                id: "9".into(),
                name: "Preset9".into()
            }]
        );
    }

    #[test]
    fn speeds_are_written_plainly_and_kept_in_range() {
        assert_eq!(speed(Some(0.5)), "0.500");
        assert_eq!(speed(Some(-3.0)), "-1.000");
        assert_eq!(speed(None), "0.000");
    }
}
