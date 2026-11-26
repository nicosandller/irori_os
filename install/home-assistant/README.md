# Irori as a Home Assistant app (staged)

This folder is the Home Assistant app definition. It is kept here so it changes with the image,
but Home Assistant installs it from a small repository of its own, the one people add to
Home Assistant's app store. This repository isn't that one.

## Setting up the app repository (once, by the owner)

1. Create a public repository, for example `irori-home-assistant`.
2. Copy `repository.yaml` to its root, with `url` pointing at the new repository, and the
   `irori/` folder beside it:

   ```
   repository.yaml
   irori/
     config.yaml
     DOCS.md
   ```

3. Make the `irori` package on ghcr.io public, once (github.com → your profile → Packages →
   irori → Package settings → Change visibility). A new package starts private, and Home
   Assistant can't pull a private image.
4. In Home Assistant: Settings → Apps → App store → ⋮ → Repositories, and add the new
   repository's URL.

## On every release

The release workflow publishes `ghcr.io/nicosandller/irori:<version>`. **After** it has, set
`version` in the app repository's `irori/config.yaml` to that version and push. Home Assistant
then offers the update. Bumping it before the image exists offers users an update that can't
be downloaded.

## Not yet

- An `icon.png` and `logo.png` (Home Assistant shows a placeholder until they're added).
- Using Home Assistant's app token (`SUPERVISOR_TOKEN`), so nothing has to be pasted.
- Ingress (Irori in Home Assistant's sidebar).
