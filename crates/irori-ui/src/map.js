// The map behind `map.rs`: OpenFreeMap's vector tiles, drawn by MapLibre GL JS.
//
// Everything here is fetched only once a map is on the page, and all of it is an extra: with
// no internet `mount` says "offline" and the coordinates under the map carry on by themselves.
//
// MapLibre comes from a CDN rather than out of the binary. It is 800 KB that nothing else in
// IroriOS needs, and the map can't draw without the internet anyway. The version is pinned and
// both files are checked against the hashes below (subresource integrity), so what runs on
// this page is exactly the release that was read, or nothing.
(function () {
  const BASE = 'https://unpkg.com/maplibre-gl@4.7.1/dist/';
  const SCRIPT = 'sha384-SYKAG6cglRMN0RVvhNeBY0r3FYKNOJtznwA0v7B5Vp9tr31xAHsZC0DqkQ/pZDmj';
  const SHEET = 'sha384-MinO0mNliZ3vwppuPOUnGa+iq619pfMhLVUXfC4LHwSCvF9H+6P/KO4Q7qBOYV5V';
  const STYLES = 'https://tiles.openfreemap.org/styles/';

  // The styles are OpenFreeMap's Positron (light page) and Dark (dark page), with the ground
  // and the water moved onto Irori's own paper and ink so the map sits in the page.
  const TONES = {
    light: { ground: '#f3eee8', water: '#d9d6d1', green: '#e7e6dc', built: '#ebe5de' },
    // Darker than the page: the stylesheet lifts the whole dark map so its streets read.
    dark: { ground: '#17120f', water: '#0d0a08', green: '#1b1712', built: '#130f0c' },
  };

  let loading = null;
  function library() {
    if (window.maplibregl) return Promise.resolve();
    if (loading) return loading;
    loading = new Promise((done, failed) => {
      const sheet = document.createElement('link');
      sheet.rel = 'stylesheet';
      sheet.href = BASE + 'maplibre-gl.css';
      sheet.integrity = SHEET;
      sheet.crossOrigin = 'anonymous';
      document.head.appendChild(sheet);
      const script = document.createElement('script');
      script.src = BASE + 'maplibre-gl.js';
      script.integrity = SCRIPT;
      script.crossOrigin = 'anonymous';
      script.onload = done;
      script.onerror = () => {
        // Let the next map try again: the browser may be back online by then.
        loading = null;
        script.remove();
        failed(new Error('the map library could not be loaded'));
      };
      document.head.appendChild(script);
    });
    return loading;
  }

  async function style(dark) {
    const answer = await fetch(STYLES + (dark ? 'dark' : 'positron'));
    if (!answer.ok) throw new Error('the map style answered ' + answer.status);
    const sheet = await answer.json();
    const tone = dark ? TONES.dark : TONES.light;
    for (const layer of sheet.layers) {
      const paint = (layer.paint = layer.paint || {});
      if (layer.type === 'background') paint['background-color'] = tone.ground;
      if (layer.type !== 'fill') continue;
      if (layer.id === 'water') paint['fill-color'] = tone.water;
      else if (/park|wood|grass/.test(layer.id)) paint['fill-color'] = tone.green;
      else if (/residential|building|pier/.test(layer.id)) paint['fill-color'] = tone.built;
    }
    return sheet;
  }

  // Irori's mark on a stem. The outer element is MapLibre's to place; the inner one is the
  // page's to move (it leans, lifts and lands by its own `translate` and `scale`).
  function pinElement() {
    const at = document.createElement('div');
    at.className = 'map-pin-at';
    at.innerHTML =
      '<div class="map-pin"><span class="map-pin-ring" aria-hidden="true"></span>' +
      '<svg viewBox="0 0 48 60" aria-hidden="true">' +
      '<path class="map-pin-stem" d="M24 58 14 44h20Z"/>' +
      '<rect class="map-pin-frame" x="4" y="4" width="40" height="40" rx="4"/>' +
      '<rect class="map-pin-ember" x="17" y="17" width="14" height="14" rx="1.5"/>' +
      '</svg></div>';
    return at;
  }

  // Put down: the same animation each time, started afresh.
  function land(at) {
    const pin = at.firstChild;
    pin.classList.remove('held');
    pin.classList.toggle('landed-a');
    pin.classList.toggle('landed-b', !pin.classList.contains('landed-a'));
  }

  window.iroriMap = {
    // `options`: { latitude, longitude, zoom, pin: [latitude, longitude] | null, dark,
    // editable }. `onPin(latitude, longitude)` is the pin put somewhere by hand; `onState` is
    // told "ready" once the map has drawn, or "offline" if it can't.
    mount(element, options, onPin, onState) {
      let map = null;
      let marker = null;
      let sizes = null;
      let gone = false;

      const place = (latitude, longitude) => {
        if (!marker) {
          const at = pinElement();
          marker = new maplibregl.Marker({
            element: at,
            anchor: 'bottom',
            draggable: !!options.editable,
          });
          marker.on('dragstart', () => at.firstChild.classList.add('held'));
          marker.on('dragend', () => {
            land(at);
            const now = marker.getLngLat();
            onPin(now.lat, now.lng);
          });
        }
        marker.setLngLat([longitude, latitude]).addTo(map);
      };

      const ready = (async () => {
        await library();
        const sheet = await style(options.dark);
        if (gone) return;
        map = new maplibregl.Map({
          container: element,
          style: sheet,
          center: [options.longitude, options.latitude],
          zoom: options.zoom,
          minZoom: 1,
          maxZoom: 18,
          attributionControl: { compact: true },
          dragRotate: false,
          pitchWithRotate: false,
        });
        map.touchZoomRotate.disableRotation();
        // The frame is often still opening when the map is made (a Settings row rolling
        // down, a step sliding in): the map takes its size again whenever the frame's changes.
        sizes = new ResizeObserver(() => map && map.resize());
        sizes.observe(element);
        map.once('load', () => onState('ready'));
        if (options.pin) place(options.pin[0], options.pin[1]);
        if (options.editable) {
          map.on('click', (event) => {
            place(event.lngLat.lat, event.lngLat.lng);
            land(marker.getElement());
            onPin(event.lngLat.lat, event.lngLat.lng);
          });
        }
      })();
      ready.catch((error) => {
        console.error(error);
        if (!gone) onState('offline');
      });
      // Whatever is asked before the map is there waits for it; after a failure, nothing.
      const then = (work) => ready.then(() => map && !gone && work()).catch(() => {});

      return {
        // The pin moved from outside the map: a search result, typed coordinates. `null`
        // takes it off.
        setPin(latitude, longitude) {
          then(() => {
            if (latitude === null || longitude === null) {
              if (marker) marker.remove();
              return;
            }
            const now = marker && marker._map ? marker.getLngLat() : null;
            if (now && Math.abs(now.lat - latitude) < 1e-7 && Math.abs(now.lng - longitude) < 1e-7) {
              return;
            }
            place(latitude, longitude);
            land(marker.getElement());
          });
        },
        // Travel to a place, or simply be there when the page has been asked to keep still.
        flyTo(latitude, longitude, zoom, still) {
          then(() => {
            const to = { center: [longitude, latitude], zoom };
            if (still) map.jumpTo(to);
            else map.flyTo({ ...to, speed: 1.4, curve: 1.5, essential: true });
          });
        },
        zoomBy(steps, still) {
          then(() => map.easeTo({ zoom: Math.round(map.getZoom() + steps), duration: still ? 0 : 340 }));
        },
        setDark(dark) {
          then(async () => {
            const sheet = await style(dark);
            if (!gone) map.setStyle(sheet);
          });
        },
        destroy() {
          gone = true;
          if (sizes) sizes.disconnect();
          if (map) map.remove();
          map = null;
        },
      };
    },
  };
})();
