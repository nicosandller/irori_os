# Cameras

IP cameras on your network, over ONVIF and RTSP, with extras for Reolink.

- **Finding them.** Cameras that speak ONVIF answer a WS-Discovery probe on the local network.
  Where that probe doesn't reach (a container's network, say), list the camera's address under
  `hosts` in the settings.
- **Logging in.** Each camera needs its own login. It shows up under **+ Add device** asking for a
  username and password, and they're kept in `secrets.toml`. For a Tapo camera, make the login in
  the Tapo app: the camera's Advanced settings, then Camera account.
- **A wrong login is tried once.** Cameras lock an account after a few failed logins, so a login
  a camera refused is never sent again, even after a restart, until you type a different one.
- **Everything stays local.** No vendor account and no cloud call.
