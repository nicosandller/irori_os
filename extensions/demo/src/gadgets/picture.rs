//! What the demo's cameras see, drawn as an SVG: the scene each one looks at, lit for the hour
//! (grey and grainy at night, as a camera's infrared is), someone in it while there's motion,
//! and the camera's name, the time and a red dot while it records, as a real feed shows them.

use std::fmt::Write as _;

use irori_protocol::Picture;

use super::camera::Scene;
use crate::world::during;

/// One picture's worth of what's going on.
pub(super) struct Shot<'a> {
    pub(super) scene: Scene,
    pub(super) name: &'a str,
    pub(super) hour: f64,
    pub(super) on: bool,
    pub(super) recording: bool,
    /// Someone (or something) in front of it, whether or not it's watching for motion.
    pub(super) someone: bool,
}

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// The picture, as a camera's protocol hands it over.
pub(super) fn draw(shot: &Shot<'_>) -> Picture {
    Picture {
        content_type: "image/svg+xml".to_owned(),
        bytes: svg(shot).into_bytes(),
    }
}

/// How much daylight there is, 0 at night to 1 at noon.
fn light(hour: f64) -> f64 {
    ((hour - 6.0) / 14.0 * std::f64::consts::PI).sin().max(0.0)
}

/// A colour between `night` and `day`, by how much daylight there is.
fn lit(night: (u8, u8, u8), day: (u8, u8, u8), amount: f64) -> String {
    let mix = |a: u8, b: u8| {
        let value = f64::from(a) + (f64::from(b) - f64::from(a)) * amount;
        // In 0..=255: a mix of two bytes by an amount in 0..=1.
        value.round().clamp(0.0, 255.0) as u8
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        mix(night.0, day.0),
        mix(night.1, day.1),
        mix(night.2, day.2)
    )
}

fn clock(hour: f64) -> String {
    let minutes = (hour * 60.0).round() as u32 % (24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Text as it can sit in the SVG.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn svg(shot: &Shot<'_>) -> String {
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}" font-family="ui-monospace, Menlo, monospace">"#
    );
    if shot.on {
        let indoors = matches!(shot.scene, Scene::Garage | Scene::LivingRoom);
        // Indoors keeps a lamp's worth of light at night; outdoors goes to the camera's own.
        let day = if indoors {
            0.55 + 0.45 * light(shot.hour)
        } else {
            light(shot.hour)
        };
        let night = day < 0.25;
        // At night the camera sees by its own infrared: the scene in greys with some contrast,
        // under a sky that stays dark.
        let shade = if night { 0.6 } else { day };
        out.push_str(r#"<g"#);
        if night {
            out.push_str(r#" filter="url(#ir)""#);
        }
        out.push('>');
        scene(&mut out, shot.scene, Light { shade, sky: day }, shot.hour);
        if shot.someone {
            figure(&mut out, shot.scene, shade);
        }
        out.push_str("</g>");
        if night {
            out.push_str(concat!(
                r#"<defs><filter id="ir"><feColorMatrix type="saturate" values="0"/>"#,
                r#"<feComponentTransfer><feFuncR type="linear" slope=".85" intercept="0.02"/>"#,
                r#"<feFuncG type="linear" slope=".85" intercept="0.02"/>"#,
                r#"<feFuncB type="linear" slope=".85" intercept="0.02"/></feComponentTransfer>"#,
                r#"</filter></defs>"#
            ));
        }
        overlay(&mut out, shot, night);
    } else {
        let _ = write!(
            out,
            r##"<rect width="{WIDTH}" height="{HEIGHT}" fill="#0d0d0f"/><text x="320" y="186" fill="#77757a" font-size="18" text-anchor="middle">Camera off</text>"##
        );
        caption(&mut out, shot);
    }
    out.push_str("</svg>");
    out
}

/// The camera's name and the time, top left, and a red dot while it records.
fn overlay(out: &mut String, shot: &Shot<'_>, night: bool) {
    caption(out, shot);
    if night {
        let _ = write!(
            out,
            r##"<text x="{}" y="{}" fill="#ffffff" fill-opacity=".75" font-size="13" text-anchor="end">IR</text>"##,
            WIDTH - 16,
            HEIGHT - 16
        );
    }
    if shot.recording {
        let _ = write!(
            out,
            r##"<circle cx="{}" cy="26" r="6" fill="#e5484d"/><text x="{}" y="31" fill="#ffffff" font-size="14" text-anchor="end">REC</text>"##,
            WIDTH - 66,
            WIDTH - 16
        );
    }
}

fn caption(out: &mut String, shot: &Shot<'_>) {
    let _ = write!(
        out,
        r##"<rect x="10" y="10" width="{}" height="26" rx="4" fill="#000000" fill-opacity=".45"/><text x="18" y="28" fill="#ffffff" font-size="14">{} · {}</text>"##,
        (shot.name.chars().count() as u32 + 9) * 9,
        escaped(shot.name),
        clock(shot.hour)
    );
}

/// How lit a scene is drawn: everything in it, and the sky above, which stays dark at night
/// whatever the camera's infrared shows of the ground.
#[derive(Debug, Clone, Copy)]
struct Light {
    shade: f64,
    sky: f64,
}

fn scene(out: &mut String, scene: Scene, light: Light, hour: f64) {
    let day = light.shade;
    match scene {
        // The car is out from the morning until the evening.
        Scene::Garage => garage(out, day, !during(hour, &[(8.35, 17.45)])),
        Scene::LivingRoom => living_room(out, day),
        Scene::Lawn => lawn(out, light),
        Scene::Shed => shed(out, light),
        Scene::Doorstep => doorstep(out, light),
    }
}

fn sky(out: &mut String, day: f64, horizon: u32) {
    let _ = write!(
        out,
        r#"<rect width="{WIDTH}" height="{horizon}" fill="{}"/>"#,
        lit((14, 20, 40), (150, 196, 232), day)
    );
}

fn garage(out: &mut String, day: f64, car_home: bool) {
    let wall = lit((40, 40, 44), (176, 172, 164), day);
    let floor = lit((30, 30, 32), (120, 118, 114), day);
    let car = lit((30, 40, 60), (60, 92, 150), day);
    let _ = write!(
        out,
        concat!(
            r#"<rect width="640" height="360" fill="{wall}"/>"#,
            r#"<polygon points="0,250 640,250 640,360 0,360" fill="{floor}"/>"#,
            r##"<rect x="40" y="60" width="140" height="90" fill="#3a3632" opacity=".5"/>"##,
            r##"<path d="M60 75h100M60 100h100M60 125h100" stroke="#2a2724" stroke-width="4"/>"##,
        ),
        wall = wall,
        floor = floor,
    );
    if !car_home {
        // An oil mark where it stands.
        out.push_str(r##"<ellipse cx="400" cy="300" rx="110" ry="14" fill="#000000" opacity=".12"/>"##);
        return;
    }
    let _ = write!(
        out,
        concat!(
            r#"<rect x="250" y="180" width="300" height="90" rx="30" fill="{car}"/>"#,
            r#"<rect x="300" y="140" width="190" height="60" rx="20" fill="{car}"/>"#,
            r##"<rect x="315" y="150" width="70" height="40" rx="8" fill="#b9d4ee" opacity=".6"/>"##,
            r##"<rect x="400" y="150" width="75" height="40" rx="8" fill="#b9d4ee" opacity=".6"/>"##,
            r##"<circle cx="310" cy="272" r="26" fill="#1b1b1d"/><circle cx="490" cy="272" r="26" fill="#1b1b1d"/>"##
        ),
        car = car
    );
}

fn living_room(out: &mut String, day: f64) {
    let wall = lit((52, 46, 44), (226, 214, 198), day);
    let floor = lit((40, 32, 26), (160, 120, 86), day);
    let window = lit((20, 28, 52), (170, 210, 240), light_outside(day));
    let _ = write!(
        out,
        concat!(
            r#"<rect width="640" height="360" fill="{wall}"/>"#,
            r#"<rect y="270" width="640" height="90" fill="{floor}"/>"#,
            r##"<rect x="440" y="50" width="150" height="130" fill="{window}" stroke="#ece6dc" stroke-width="8"/>"##,
            r##"<rect x="60" y="110" width="200" height="115" rx="4" fill="#141416"/>"##,
            r##"<rect x="150" y="225" width="20" height="25" fill="#2b2b2e"/><rect x="90" y="248" width="140" height="12" fill="#5b4636"/>"##,
            r##"<rect x="300" y="210" width="260" height="70" rx="18" fill="#6c4d3b"/>"##,
            r##"<rect x="300" y="185" width="260" height="40" rx="16" fill="#7b5a46"/>"##
        ),
        wall = wall,
        floor = floor,
        window = window
    );
}

/// Indoors the room keeps some light at night; the window shows the sky as it is.
fn light_outside(day: f64) -> f64 {
    ((day - 0.55) / 0.45).clamp(0.0, 1.0)
}

fn lawn(out: &mut String, light: Light) {
    let day = light.shade;
    sky(out, light.sky, 170);
    let grass = lit((16, 34, 20), (98, 160, 72), day);
    let hedge = lit((10, 24, 14), (52, 104, 50), day);
    let _ = write!(
        out,
        concat!(
            r#"<rect y="140" width="640" height="40" fill="{hedge}"/>"#,
            r#"<rect y="170" width="640" height="190" fill="{grass}"/>"#,
            r##"<rect x="470" y="70" width="18" height="110" fill="#5a3f2b"/>"##,
            r#"<circle cx="480" cy="70" r="55" fill="{hedge}"/>"#,
            r##"<rect x="90" y="230" width="120" height="10" rx="3" fill="#8a7a62"/>"##,
            r##"<path d="M100 240v30M200 240v30" stroke="#6b5b45" stroke-width="6"/>"##
        ),
        hedge = hedge,
        grass = grass
    );
}

fn shed(out: &mut String, light: Light) {
    let day = light.shade;
    sky(out, light.sky, 190);
    let grass = lit((16, 30, 18), (110, 158, 80), day);
    let wood = lit((36, 28, 22), (140, 104, 70), day);
    let _ = write!(
        out,
        concat!(
            r#"<rect y="190" width="640" height="170" fill="{grass}"/>"#,
            r#"<rect x="300" y="110" width="220" height="150" fill="{wood}"/>"#,
            r##"<polygon points="290,115 410,55 530,115" fill="#4a3a30"/>"##,
            r##"<rect x="380" y="170" width="60" height="90" fill="#3b2c22"/>"##,
            r##"<path d="M40 200v-80M80 200v-80M120 200v-80M20 140h120M20 175h120" stroke="#8c7b62" stroke-width="5"/>"##
        ),
        grass = grass,
        wood = wood
    );
}

fn doorstep(out: &mut String, light: Light) {
    let day = light.shade;
    sky(out, light.sky, 150);
    let path = lit((40, 38, 36), (178, 168, 152), day);
    let lawn = lit((16, 32, 20), (96, 150, 72), day);
    let _ = write!(
        out,
        concat!(
            r#"<rect y="150" width="640" height="210" fill="{lawn}"/>"#,
            r#"<polygon points="270,150 370,150 520,360 120,360" fill="{path}"/>"#,
            r##"<rect x="40" y="110" width="80" height="60" fill="#3d4a5a"/>"##,
            r##"<rect x="520" y="120" width="60" height="70" rx="4" fill="#5e5348"/>"##,
            r##"<rect x="534" y="104" width="32" height="20" rx="3" fill="#2f2a25"/>"##
        ),
        lawn = lawn,
        path = path
    );
}

/// Someone in the picture, where they'd be in this scene.
fn figure(out: &mut String, scene: Scene, day: f64) {
    let (x, y, scale) = match scene {
        Scene::Garage => (190.0, 270.0, 1.0),
        Scene::LivingRoom => (360.0, 280.0, 1.05),
        Scene::Lawn => (300.0, 300.0, 0.9),
        Scene::Shed => (250.0, 280.0, 0.8),
        Scene::Doorstep => (320.0, 330.0, 1.25),
    };
    let body = lit((70, 70, 74), (44, 52, 64), day);
    let _ = write!(
        out,
        concat!(
            r#"<g transform="translate({x} {y}) scale({scale})" fill="{body}">"#,
            r#"<circle cx="0" cy="-150" r="18"/>"#,
            r#"<rect x="-24" y="-130" width="48" height="80" rx="18"/>"#,
            r#"<rect x="-20" y="-60" width="16" height="60" rx="7"/>"#,
            r#"<rect x="4" y="-60" width="16" height="60" rx="7"/>"#,
            r#"</g>"#
        ),
        x = x,
        y = y,
        scale = scale,
        body = body
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_says_whose_it_is_when_and_whether_it_records() {
        let shot = |scene, hour, recording| {
            String::from_utf8(
                draw(&Shot {
                    scene,
                    name: "Garden camera 1",
                    hour,
                    on: true,
                    recording,
                    someone: recording,
                })
                .bytes,
            )
            .expect("text")
        };
        let noon = shot(Scene::Lawn, 12.5, true);
        assert!(noon.starts_with("<svg") && noon.ends_with("</svg>"));
        assert!(noon.contains("Garden camera 1 · 12:30"));
        assert!(noon.contains("REC") && !noon.contains("IR<"));
        let night = shot(Scene::Shed, 2.0, false);
        assert!(night.contains("url(#ir)") && !night.contains("REC"));
        for scene in [
            Scene::Garage,
            Scene::LivingRoom,
            Scene::Lawn,
            Scene::Shed,
            Scene::Doorstep,
        ] {
            let drawn = draw(&Shot {
                scene,
                name: "<Cam & co>",
                hour: 18.0,
                on: true,
                recording: true,
                someone: true,
            });
            assert_eq!(drawn.check(), Ok(()));
            let text = String::from_utf8(drawn.bytes).expect("text");
            assert!(text.contains("&lt;Cam &amp; co&gt;"), "{text}");
            assert!(!text.contains("<script"));
        }
        assert_eq!(clock(23.99), "23:59");
    }
}
