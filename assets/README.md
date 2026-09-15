# IroriOS brand assets

Two marks: **A** (sunken hearth) and **B** (geometric flame). All SVG, no raster needed.

**Mark A is the one in use**: README banner, web UI favicon and header, and the `irori serve`
terminal banner. B is kept as the alternative; switching means swapping the `-a` files for
`-b` in those places (`README.md`, `crates/irori/assets/`, `crates/irori/src/banner.rs`).

Naming: **IroriOS** is the brand (logo, wordmark); **Irori** / `irori` is the program.

| File | Use |
| --- | --- |
| `irori-mark-{a,b}.svg` | the mark alone, 48×48, transparent |
| `irori-mark-{a,b}-mono.svg` | one-colour, uses `currentColor` |
| `irori-mark-{a,b}-knockout.svg` | for dark backgrounds |
| `favicon-{a,b}.svg` | favicon (32px intrinsic, scales) |
| `irori-avatar-{a,b}.svg` + `-dark` | GitHub org/repo avatar, 512×512 |
| `irori-lockup-{a,b}.svg` + `-dark` | horizontal mark + wordmark |
| `irori-stacked-{a,b}.svg` | stacked lockup |
| `irori-banner-{a,b}.svg` | README header, 1280×320 |
| `irori-og-{a,b}.svg` | social / OG card, 1200×630 |
| `irori-cli-art.txt` | terminal banner |

Tokens: ink `#1c1714` · paper `#faf7f4` · ember `#c4552b` · muted `#9a8f86`.
Type: IBM Plex Sans (wordmark, 500) and IBM Plex Mono (labels).

## README header

```md
<p align="center"><img src="assets/irori-banner-a.svg" alt="IroriOS" width="640"></p>
```

## Favicon

```html
<link rel="icon" href="/favicon-a.svg" type="image/svg+xml">
```

## Notes

- Lockups and banners use `<text>`; install IBM Plex or convert text to outlines if you need byte-identical rendering everywhere. The marks themselves are pure geometry and render identically anywhere.
- Clear space around the mark: one stroke-width (1/12 of the mark's height) on all sides.
- Minimum mark size: 16px. Below that, drop the wordmark.
