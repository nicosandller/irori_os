# ONVIF Cameras

IP cameras on your network, over ONVIF and RTSP, with extras for Reolink.

- **Finding them.** Cameras that speak ONVIF answer a WS-Discovery probe on the local network.
  Where that probe doesn't reach (a container's network, say), list the camera's address under
  `hosts` in the settings.
- **Logging in.** Each camera needs its own login. It shows up under **+ Add device** asking for a
  username and password, and they're kept in `secrets.toml`. For a Tapo camera, make the login in
  the Tapo app: the camera's Advanced settings, then Camera account.
- **A wrong login is tried once.** Cameras lock an account after a few failed logins, so a login
  a camera refused is never sent again, even after a restart, until you type a different one.
- **What you get.** Each camera's live video (SD and HD, with sound where the browser can play
  it), a picture when it has one, a sensor for each thing it notices (motion, and on a Reolink
  people, vehicles and animals), and arrows, zoom and presets on cameras that turn.
- **Reolink extras.** A Reolink's spotlight (with brightness), siren, infrared lights and status
  light, through its own API with the same login, over HTTPS. Each appears only when the camera
  has it. The siren sounds for 10 seconds unless told how long, and never more than 5 minutes. If
  the API turns the login down (an account allowed ONVIF but not the API, say), the camera still
  works without them, and the login isn't tried there again until the extension restarts.
- **Everything stays local.** No vendor account and no cloud call.
