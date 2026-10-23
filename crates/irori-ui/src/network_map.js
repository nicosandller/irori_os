// The network map behind `network_map.rs`: every protocol's network drawn around Irori, alive.
//
// The devices hold still, so they are easy to point at; the motion is in the links and the
// space around them. A link hums faster the more is passing along it, and a shockwave rattles
// every link it crosses. Behind it all is a backdrop: a grid of dots that bends under the
// shockwaves and leans toward the pointer, or a field of where each network's signal reaches,
// with the dead zones tinted. Each message can be a note, pitched by its network.
//
// The page (Rust) owns the data and the side panel. This owns the drawing: it lays the devices
// out, as a mesh or in rings by hop count, and draws them in SVG over a canvas. With motion off
// nothing moves: the map is drawn once whenever it changes, and messages aren't shown arriving.
(function () {
  const TAU = Math.PI * 2;
  const NS = 'http://www.w3.org/2000/svg';
  const W = 1000, H = 640, CX = 500, CY = 320, RX = 115, RY = 72;

  function svg(tag, attrs, parent) {
    const node = document.createElementNS(NS, tag);
    for (const key in attrs || {}) node.setAttribute(key, attrs[key]);
    if (parent) parent.appendChild(node);
    return node;
  }
  // Where devices were dragged to, by layout and then by node id.
  const PLACES = 'irori.network.places';
  function loadPlaces() {
    try {
      const saved = JSON.parse(window.localStorage.getItem(PLACES) || '{}');
      return saved && typeof saved === 'object' ? saved : {};
    } catch (error) {
      return {};
    }
  }
  const ekey = (a, b) => (a < b ? a + '|' + b : b + '|' + a);
  const bez = (A, C, B, t) => {
    const u = 1 - t;
    return { x: u * u * A.x + 2 * u * t * C.x + t * t * B.x, y: u * u * A.y + 2 * u * t * C.y + t * t * B.y };
  };
  // A colour token as numbers, so it can be drawn at any alpha. Tokens are hex or rgb().
  function rgbOf(color) {
    const text = (color || '').trim();
    if (text.startsWith('#')) {
      const hex = text.length === 4 ? text.slice(1).split('').map((c) => c + c).join('') : text.slice(1, 7);
      const n = parseInt(hex, 16);
      return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
    }
    const m = text.match(/(\d+(\.\d+)?)/g);
    return m ? m.slice(0, 3).map(Number) : [128, 128, 128];
  }
  const rgba = (rgb, a) => 'rgba(' + rgb[0] + ',' + rgb[1] + ',' + rgb[2] + ',' + a + ')';

  // Where each network gathers on the mesh: one in the middle, two side by side, three like
  // a triangle under Irori, more around it.
  function anchors(count) {
    if (count <= 1) return [{ x: 500, y: 330 }];
    if (count === 2) return [{ x: 320, y: 320 }, { x: 680, y: 320 }];
    if (count === 3) return [{ x: 320, y: 290 }, { x: 700, y: 290 }, { x: 500, y: 545 }];
    return Array.from({ length: count }, (_, i) => {
      const a = Math.PI + (i * TAU) / count;
      return { x: 500 + Math.cos(a) * 310, y: 320 + Math.sin(a) * 215 };
    });
  }

  function depthsOf(nodes, byId) {
    const depth = {};
    for (const n of nodes) {
      let d = 0, at = n, seen = new Set();
      while (at && at.parent && byId[at.parent] && !seen.has(at.id)) { seen.add(at.id); d++; at = byId[at.parent]; }
      depth[n.id] = d;
    }
    return depth;
  }

  function anchorOf(n, spots) {
    const nets = n.nets && n.nets.length ? n.nets : [n.net];
    let x = 0, y = 0;
    for (const i of nets) { const s = spots[i] || spots[0]; x += s.x; y += s.y; }
    x /= nets.length; y /= nets.length;
    // Shared by several networks (the Wi-Fi): between them, and below Irori.
    if (nets.length > 1) y = Math.max(y, 470);
    return { x, y };
  }

  function meshLayout(d) {
    const { nodes, links, byId } = d;
    const spots = anchors(d.networks.length);
    const N = nodes.length, crowd = Math.min(1, Math.sqrt(30 / Math.max(1, N)));
    const P = {};
    nodes.forEach((n, i) => {
      const a = i * 2.39996, r = 60 + i * 14 * crowd, home = anchorOf(n, spots);
      P[n.id] = { x: home.x + Math.cos(a) * r * 0.6, y: home.y + Math.sin(a) * r * 0.4, vx: 0, vy: 0, home };
    });
    const hub = nodes.find((n) => n.kind === 'hub');
    if (hub) { P[hub.id].x = CX; P[hub.id].y = CY; }
    const ids = nodes.map((n) => n.id), rounds = N > 80 ? 300 : 600;
    const repel = 6500 * crowd * crowd;
    for (let it = 0; it < rounds; it++) {
      const cool = 1 - it / rounds, F = {};
      for (const id of ids) F[id] = { x: 0, y: 0 };
      for (let i = 0; i < N; i++) for (let j = i + 1; j < N; j++) {
        const a = P[ids[i]], b = P[ids[j]];
        const dx = a.x - b.x, dy = a.y - b.y, d2 = Math.max(dx * dx + dy * dy, 100), dd = Math.sqrt(d2), f = repel / d2;
        F[ids[i]].x += (dx / dd) * f; F[ids[i]].y += (dy / dd) * f; F[ids[j]].x -= (dx / dd) * f; F[ids[j]].y -= (dy / dd) * f;
      }
      for (const e of links) {
        const a = P[e.a], b = P[e.b], A = byId[e.a], B = byId[e.b];
        if (!a || !b) continue;
        const routers = (A.kind === 'router' || A.kind === 'coord') && B.kind === 'router';
        const L = (e.look === 'wired' ? 105 : routers ? 125 : 78) * (0.6 + 0.4 * crowd);
        const dx = b.x - a.x, dy = b.y - a.y, dd = Math.max(1, Math.hypot(dx, dy)), k = (dd - L) * 0.04;
        F[e.a].x += (dx / dd) * k; F[e.a].y += (dy / dd) * k; F[e.b].x -= (dx / dd) * k; F[e.b].y -= (dy / dd) * k;
      }
      for (const n of nodes) {
        const p = P[n.id];
        F[n.id].x += (p.home.x - p.x) * 0.006;
        F[n.id].y += (p.home.y - p.y) * 0.009;
      }
      for (const id of ids) {
        if (hub && id === hub.id) continue;
        const p = P[id];
        p.vx = (p.vx + F[id].x) * 0.82; p.vy = (p.vy + F[id].y) * 0.82;
        p.x = Math.max(70, Math.min(930, p.x + p.vx * cool)); p.y = Math.max(50, Math.min(585, p.y + p.vy * cool));
      }
    }
    const out = {};
    for (const id of ids) out[id] = { x: P[id].x, y: P[id].y };
    return out;
  }

  function ringsLayout(d) {
    const { nodes, byId, depth } = d;
    const count = d.networks.length, spots = anchors(count);
    const deepest = Math.max(1, ...nodes.map((n) => depth[n.id]));
    const scale = Math.min(1, 4 / deepest);
    const centre = (n) => { const s = anchorOf(n, spots); return Math.atan2(s.y - CY, s.x - CX); };
    const span = count <= 1 ? 5.6 : Math.min(2.4, (TAU / count) * 0.85);
    const ang = {}, out = {};
    for (const n of nodes) if (n.kind === 'hub') { ang[n.id] = 0; out[n.id] = { x: CX, y: CY }; }
    for (let dep = 1; dep <= deepest; dep++) {
      // Each network's sector, its devices in the order of their parents round the ring.
      const groups = {};
      for (const n of nodes) {
        if (depth[n.id] !== dep || n.kind === 'hub') continue;
        const key = (n.nets || [n.net]).join(',');
        (groups[key] = groups[key] || []).push(n);
      }
      for (const key in groups) {
        const list = groups[key].sort((a, b) => ((ang[a.parent] || 0) - (ang[b.parent] || 0)) || nodes.indexOf(a) - nodes.indexOf(b));
        const c = centre(list[0]), shared = key.includes(',');
        const width = shared ? 0.35 : span, step = 0.62 / Math.sqrt(dep), s = Math.min(width, (list.length - 1) * step);
        // Going round the left of the ring, the order flips, so parents stay beside children.
        const flip = Math.cos(c) < -0.3;
        list.forEach((n, i) => {
          const f = list.length === 1 ? 0.5 : i / (list.length - 1);
          const a = c + (flip ? s / 2 - s * f : -s / 2 + s * f);
          ang[n.id] = a;
          out[n.id] = { x: CX + Math.cos(a) * dep * RX * scale, y: CY + Math.sin(a) * dep * RY * scale };
        });
      }
    }
    for (const n of nodes) if (!out[n.id]) out[n.id] = { x: CX, y: CY };
    return { at: out, rings: deepest, scale };
  }

  window.iroriNetwork = {
    mount(root, options, onSelect) {
      root.textContent = '';
      const back = svg('svg', { viewBox: '0 0 ' + W + ' ' + H, class: 'nm-layer' }, root);
      const ground = svg('rect', { x: 0, y: 0, width: W, height: H, fill: 'transparent' }, back);
      ground.addEventListener('click', () => onSelect(null));
      const ringsG = svg('g', { class: 'nm-rings', fill: 'none' }, back);
      const edgesG = svg('g', { class: 'nm-edges' }, back);
      const canvas = document.createElement('canvas');
      canvas.className = 'nm-canvas';
      root.appendChild(canvas);
      const front = svg('svg', { viewBox: '0 0 ' + W + ' ' + H, class: 'nm-layer nm-front' }, root);
      const defs = svg('defs', {}, front);
      const lit = svg('radialGradient', { id: 'nm-lit-' + Math.random().toString(36).slice(2) }, defs);
      svg('stop', { offset: '0', 'stop-color': 'var(--nm-lit)', 'stop-opacity': '0.75' }, lit);
      svg('stop', { offset: '0.5', 'stop-color': 'var(--nm-lit)', 'stop-opacity': '0.22' }, lit);
      svg('stop', { offset: '1', 'stop-color': 'var(--nm-lit)', 'stop-opacity': '0' }, lit);
      const labelsG = svg('g', { class: 'nm-labels' }, front);
      const nodesG = svg('g', { class: 'nm-nodes' }, front);
      const ctx = canvas.getContext('2d');

      const s = {
        d: null, layout: options.layout === 'rings' ? 'rings' : 'mesh', filter: null, query: '', labels: false,
        backdrop: options.backdrop === 'coverage' ? 'coverage' : 'grid', sound: false, motion: !!options.motion,
        active: true, sel: null, hover: null, mouse: null, t: 0, last: 0, raf: 0, gone: false,
        pos: {}, base: {}, tween: null, layouts: null, idle: options.idle !== false,
        packets: [], sparks: [], ripples: [], sweeps: [], heat: {}, hum: {}, twang: {}, pop: {}, vis: {},
        hubPulse: 0, idleAcc: 0, beatAcc: 0, timers: new Set(), covTick: 0, cov: null, covKey: '', theme: null, ac: null,
        edgeEls: {}, nodeEls: {}, labelEls: {}, dirty: true,
        // Where devices were dragged to, per layout, kept by this browser; and a drag under way.
        places: loadPlaces(), drag: null, dragged: false, client: null,
      };

      // The canvas is as sharp as the screen shows it, and no sharper: a map drawn at twice its
      // size on a screen that doesn't need it is four times the work every frame.
      let scale = 2;
      function sizeCanvas() {
        const shown = root.clientWidth || W, dpr = Math.min(2, window.devicePixelRatio || 1);
        scale = Math.max(0.5, (shown * dpr) / W);
        canvas.width = Math.round(W * scale); canvas.height = Math.round(H * scale);
        s.dirty = true;
      }
      sizeCanvas();
      const resized = window.ResizeObserver ? new ResizeObserver(() => { sizeCanvas(); paint(); }) : null;
      if (resized) resized.observe(root);

      // Writes an attribute or a style only when it changed: most of the map is the same from
      // one frame to the next, and every write costs the browser a restyle.
      const put = (el, name, value) => {
        const c = el.__nm || (el.__nm = {});
        if (c[name] !== value) { c[name] = value; el.setAttribute(name, value); }
      };
      const putStyle = (el, name, value) => {
        const c = el.__nms || (el.__nms = {});
        if (c[name] !== value) { c[name] = value; el.style[name] = value; }
      };

      // The page's colours, read from its tokens, so both themes and any change between them
      // are the page's own.
      function readTheme() {
        const css = getComputedStyle(root);
        const get = (name, fallback) => (css.getPropertyValue(name) || '').trim() || fallback;
        const tones = {};
        for (const tone of ['zigbee', 'matter', 'esphome', 'thread', 'wifi', 'other-1', 'other-2', 'other-3', 'other-4']) {
          tones[tone] = get('--nm-' + tone, '#a39990');
        }
        const t = {
          tones, ink: get('--nm-ink', '#f1ebe6'), dim: get('--nm-dim', '#a39990'), grid: get('--nm-grid', '#c9beb5'),
          weak: get('--nm-weak', '#d9a24a'), poor: get('--nm-poor', '#e0594a'), off: get('--nm-off', '#5a504a'),
          cmd: get('--nm-cmd', '#ffe0c2'), litc: get('--nm-lit', '#f2c88a'), core: get('--nm-core', '#fff6ea'),
          glow: get('--nm-glow', '#e07a52'), blend: get('--nm-blend', 'lighter'),
        };
        t.rgb = {};
        for (const key of ['ink', 'dim', 'grid', 'weak', 'poor', 'off', 'cmd', 'litc', 'core', 'glow']) t.rgb[key] = rgbOf(t[key]);
        t.toneRgb = {};
        for (const tone in tones) t.toneRgb[tone] = rgbOf(tones[tone]);
        s.theme = t;
        s.cov = null;
        s.dirty = true;
      }
      readTheme();
      const scheme = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
      const onScheme = () => { readTheme(); paint(); };
      if (scheme && scheme.addEventListener) scheme.addEventListener('change', onScheme);

      const netOf = (n) => (s.d && s.d.networks[n.net]) || null;
      const toneOf = (net) => (net && s.theme.tones[net.tone]) || s.theme.dim;
      const toneRgbOf = (net) => (net && s.theme.toneRgb[net.tone]) || s.theme.rgb.dim;
      const live = () => s.motion && s.active && !document.hidden;
      // A timer that forgets itself once it has run: every message sets a few, and a map left
      // open all day mustn't keep a list of every one.
      const later = (fn, ms) => {
        const timer = setTimeout(() => { s.timers.delete(timer); if (!s.gone) fn(); }, ms);
        s.timers.add(timer);
      };

      // Where each device goes in the layout shown: where it was dragged to, or where the layout
      // puts it.
      function placesOf() { return s.places[s.layout] || (s.places[s.layout] = {}); }
      function targetOf() {
        const laid = s.layouts ? s.layouts[s.layout] : {}, placed = placesOf(), out = {};
        for (const id in laid) out[id] = placed[id] || laid[id];
        return out;
      }
      function savePlaces() {
        try { window.localStorage.setItem(PLACES, JSON.stringify(s.places)); } catch (error) { /* kept for this visit only */ }
      }
      // Tells the page whether any device on the map now is somewhere it was dragged to.
      function arranged() {
        if (typeof options.onArranged !== 'function') return;
        const placed = placesOf();
        options.onArranged(!!s.d && s.d.nodes.some((n) => placed[n.id]));
      }

      // How a link is drawn: wired, a good link in its network's colour (heavier the better),
      // a weak one amber and dashed, a poor one red and dotted, one to a device that's away
      // grey; one nobody has measured, thin and faint.
      function edgeLook(e) {
        const t = s.theme, A = s.d.byId[e.a], B = s.d.byId[e.b];
        if ((A && A.offline) || (B && B.offline) || e.look === 'off') return { stroke: t.off, w: 1.5, dash: '2 4', lc: t.dim, word: 'Offline', q: e.q };
        if (e.look === 'wired') return { stroke: t.dim, w: 2.5, dash: '', lc: t.dim, word: 'Wired', q: 1 };
        if (e.look === 'unknown') return { stroke: t.dim, w: 1.2, dash: '1 5', lc: t.dim, word: 'Unknown', q: 0.5 };
        const far = B && B.kind !== 'hub' ? B : A;
        const c = toneOf(s.d.networks[e.net] || netOf(far));
        if (e.look === 'weak') return { stroke: t.weak, w: 1.8, dash: '6 5', lc: t.weak, word: 'Weak', q: e.q };
        if (e.look === 'poor') return { stroke: t.poor, w: 1.5, dash: '2 4', lc: t.poor, word: 'Poor', q: e.q };
        return { stroke: c, w: 1.4 + e.q * 2, dash: '', lc: t.ink, word: 'Good', q: e.q };
      }

      // Takes a new drawing. Devices that were already there stay where they were; a new one,
      // or one gone, lays the map out again, and everything eases to its new place.
      function setDrawing(drawing) {
        const before = s.d;
        const byId = {};
        for (const n of drawing.nodes) byId[n.id] = n;
        drawing.byId = byId;
        drawing.links = drawing.links.filter((e) => byId[e.a] && byId[e.b]);
        drawing.depth = depthsOf(drawing.nodes, byId);
        drawing.index = {};
        drawing.links.forEach((e, i) => { drawing.index[ekey(e.a, e.b)] = i; });
        const shape = (d) => d ? d.nodes.map((n) => n.id + '>' + (n.parent || '')).join(',') + '#' + d.links.map((e) => ekey(e.a, e.b)).join(',') : '';
        s.d = drawing;
        for (const n of drawing.nodes) if (s.pop[n.id] == null) s.pop[n.id] = 0;
        if (!before || shape(before) !== shape(drawing) || !s.layouts) {
          const from = before ? JSON.parse(JSON.stringify(s.base)) : null;
          const rings = ringsLayout(drawing);
          s.layouts = { mesh: meshLayout(drawing), rings: rings.at, ringCount: rings.rings, ringScale: rings.scale };
          if (from) {
            // Something new starts where it's going: a device dragged somewhere, where it was put.
            const going = targetOf();
            for (const n of drawing.nodes) if (!from[n.id]) from[n.id] = going[n.id];
            s.tween = { from, t0: performance.now(), dur: live() ? 650 : 0 };
          }
          buildScene();
        } else {
          refreshScene();
        }
        computePos(performance.now());
        s.cov = null;
        s.dirty = true;
        arranged();
        paint();
      }

      function buildScene() {
        const d = s.d;
        edgesG.textContent = ''; labelsG.textContent = ''; nodesG.textContent = ''; ringsG.textContent = '';
        s.edgeEls = {}; s.labelEls = {}; s.nodeEls = {};
        // The hop rings, for the rings layout.
        const scale = s.layouts.ringScale;
        for (let r = 1; r <= Math.min(4, s.layouts.ringCount); r++) {
          const step = Math.max(1, Math.round(s.layouts.ringCount / 4));
          const hops = r * step;
          if (hops > s.layouts.ringCount) break;
          svg('ellipse', { cx: CX, cy: CY, rx: hops * RX * scale, ry: hops * RY * scale, class: 'nm-ring' }, ringsG);
          const label = svg('text', { x: CX, y: CY - hops * RY * scale - 4, class: 'nm-ring-label', 'text-anchor': 'middle' }, ringsG);
          label.textContent = hops === 1 ? '1 HOP' : hops + ' HOPS';
        }
        for (const e of d.links) {
          const k = ekey(e.a, e.b);
          s.edgeEls[k] = svg('path', { fill: 'none', 'stroke-linecap': 'round', class: 'nm-edge' }, edgesG);
          const g = svg('g', { class: 'nm-edge-label' }, labelsG);
          svg('rect', { x: -26, y: -9, width: 52, height: 18, rx: 4 }, g);
          const text = svg('text', { y: 3.5, 'text-anchor': 'middle' }, g);
          text.textContent = e.label || '';
          s.labelEls[k] = { g, text, has: !!e.label };
        }
        for (const n of d.nodes) {
          const g = svg('g', { class: 'nm-node', tabindex: '-1' }, nodesG);
          const glow = svg('circle', { r: 34, fill: 'url(#' + lit.id + ')', class: 'nm-glow' }, g);
          const big = n.kind === 'hub' ? 21 : n.kind === 'coord' ? 17 : n.kind === 'router' ? 10 : 6.5;
          svg('circle', { r: Math.max(big + 8, 16), fill: 'transparent' }, g);
          const ring = svg('circle', { r: big + 7, fill: 'none', class: 'nm-selected' }, g);
          let shape = [];
          if (n.kind === 'hub') {
            shape.push(svg('rect', { x: -21, y: -21, width: 42, height: 42, rx: 11, class: 'nm-hub' }, g));
            shape.push(svg('circle', { r: 6, class: 'nm-hub-core' }, g));
          } else if (n.kind === 'coord') {
            shape.push(svg('rect', { x: -12, y: -12, width: 24, height: 24, rx: 3, transform: 'rotate(45)', class: 'nm-coord', 'stroke-width': 2.5 }, g));
            shape.push(svg('rect', { x: -4, y: -4, width: 8, height: 8, rx: 1, transform: 'rotate(45)', class: 'nm-coord-core' }, g));
          } else {
            shape.push(svg('circle', { r: n.kind === 'router' ? 10 : 6.5, class: 'nm-dev' }, g));
            shape.push(svg('circle', { r: n.kind === 'router' ? 3.5 : 0, class: 'nm-dev-core' }, g));
          }
          const label = svg('text', { y: big + 15, 'text-anchor': 'middle', class: 'nm-name' + (n.kind === 'end' || n.kind === 'sleepy' || n.kind === 'device' ? ' small' : '') }, g);
          label.textContent = n.name;
          const title = svg('title', {}, g);
          title.textContent = n.name;
          g.addEventListener('mouseenter', () => { s.hover = n.id; s.dirty = true; paint(); });
          g.addEventListener('mouseleave', () => { if (s.hover === n.id) s.hover = null; s.dirty = true; paint(); });
          g.addEventListener('pointerdown', (event) => startDrag(event, n.id));
          g.addEventListener('click', (event) => {
            event.stopPropagation();
            // The end of a drag isn't a click on what was dragged.
            if (s.dragged) { s.dragged = false; return; }
            onSelect(n.id);
          });
          s.nodeEls[n.id] = { g, glow, ring, shape, label };
        }
        refreshScene();
      }

      // The colours and shapes of what's drawn, from the drawing and the theme.
      function refreshScene() {
        const d = s.d, t = s.theme;
        for (const n of d.nodes) {
          const el = s.nodeEls[n.id];
          if (!el) continue;
          const c = toneOf(netOf(n));
          if (n.kind === 'coord') {
            el.shape[0].setAttribute('stroke', n.offline ? t.off : c);
            el.shape[1].setAttribute('fill', n.offline ? t.off : c);
          } else if (n.kind !== 'hub') {
            let fill = c, stroke = 'var(--nm-surface)', sw = 2, dash = '';
            if (n.kind === 'router') { fill = rgba(rgbOf(c), 0.2); stroke = c; }
            else if (n.kind === 'sleepy') { fill = 'var(--nm-surface)'; stroke = c; dash = '2 2'; }
            if (n.offline) { fill = 'var(--nm-surface)'; stroke = t.off; dash = '2 3'; }
            el.shape[0].setAttribute('fill', fill);
            el.shape[0].setAttribute('stroke', stroke);
            el.shape[0].setAttribute('stroke-width', sw);
            el.shape[0].setAttribute('stroke-dasharray', dash);
            el.shape[1].setAttribute('fill', c);
            el.shape[1].setAttribute('r', n.kind === 'router' && !n.offline ? 3.5 : 0);
          }
          el.label.classList.toggle('away', !!n.offline);
          el.glow.classList.toggle('on', !!n.on);
        }
        for (const e of d.links) {
          const k = ekey(e.a, e.b), lab = s.labelEls[k];
          if (lab) { lab.text.textContent = e.label || ''; lab.has = !!e.label; lab.text.setAttribute('fill', edgeLook(e).lc); }
        }
      }

      function computePos(ts) {
        let base;
        const target = targetOf();
        if (s.tween) {
          const el = ts - s.tween.t0, A = s.tween.from;
          base = {};
          let done = true;
          for (const n of s.d.nodes) {
            let k = s.tween.dur ? (el - s.d.depth[n.id] * 70) / s.tween.dur : 1;
            k = Math.max(0, Math.min(1, k));
            if (k < 1) done = false;
            const e = k >= 1 ? 1 : 1 - Math.exp(-5.5 * k) * Math.cos(7.5 * k);
            const a = A[n.id] || target[n.id], b = target[n.id];
            base[n.id] = { x: a.x + (b.x - a.x) * e, y: a.y + (b.y - a.y) * e };
          }
          if (done) s.tween = null;
        } else base = target;
        s.base = base;
        for (const n of s.d.nodes) s.pos[n.id] = base[n.id] || { x: CX, y: CY };
      }

      function routeOf(id) {
        const r = [id], seen = new Set([id]);
        let n = s.d.byId[id];
        while (n && n.parent && s.d.byId[n.parent] && !seen.has(n.parent)) { r.push(n.parent); seen.add(n.parent); n = s.d.byId[n.parent]; }
        return r;
      }

      // A link's curve: bowed a little, humming with what passes along it, and ringing a while
      // after something hits it.
      function ctrl(u, v) {
        const k = ekey(u, v), i = s.d.index[k] ?? 0, p = s.pos[u < v ? u : v], q = s.pos[u < v ? v : u];
        const dx = q.x - p.x, dy = q.y - p.y, len = Math.hypot(dx, dy) || 1;
        const hm = s.hum[k] || 0, moving = live();
        let bow = (i % 2 ? 1 : -1) * len * 0.08;
        if (moving) bow += Math.sin(s.t * (0.9 + hm * 2) + i * 1.7) * (Math.min(8, len * 0.05) + hm * 3) + Math.sin(s.t * 30 + i) * hm * 0.8;
        const tw = s.twang[k];
        if (tw && moving) {
          const age = s.t - tw.t;
          if (age < 1.4) bow += tw.amp * 0.5 * Math.exp(-age * 4.5) * Math.sin(age * 22);
          else delete s.twang[k];
        }
        return { x: (p.x + q.x) / 2 - (dy / len) * bow, y: (p.y + q.y) / 2 + (dx / len) * bow };
      }

      function ripple(id, color, maxR, dur, alpha) { if (live() && s.pos[id]) s.ripples.push({ id, color, maxR: maxR || 26, dur: dur || 900, alpha: alpha ?? 0.7, t0: performance.now() }); }
      function burst(id, color, count, speed) {
        if (!live() || !s.pos[id]) return;
        const P = s.pos[id];
        s.pop[id] = 1;
        for (let i = 0; i < count; i++) {
          const a = Math.random() * TAU, v = (50 + Math.random() * 140) * (speed || 1);
          s.sparks.push({ x: P.x, y: P.y, vx: Math.cos(a) * v, vy: Math.sin(a) * v, life: 1, c: color });
        }
      }
      function shock(id, strength, radius) {
        if (!live() || !s.pos[id]) return;
        const P = s.pos[id], reach = radius * 1.4;
        ripple(id, s.theme.ink, radius, 900, 0.4);
        for (const e of s.d.links) {
          const A = s.pos[e.a], B = s.pos[e.b];
          const dd = Math.hypot((A.x + B.x) / 2 - P.x, (A.y + B.y) / 2 - P.y);
          if (dd < reach) later(() => { s.twang[ekey(e.a, e.b)] = { amp: 5 + strength * 0.32 * (1 - dd / reach), t: s.t }; }, dd * 1.8);
        }
      }

      function blip(net, low) {
        if (!s.sound || !s.ac) return;
        const now = s.ac.currentTime;
        if (now - (s.lastBlip || 0) < 0.045) return;
        s.lastBlip = now;
        const notes = [659, 880, 523, 587, 784, 698];
        const fq = net === 'cmd' ? 988 : net === 'fail' ? 196 : net === 'hello' ? 1319 : notes[net % notes.length] || 740;
        for (const fr of [fq, low ? fq / 2 : 0]) {
          if (!fr) continue;
          const o = s.ac.createOscillator(), g = s.ac.createGain();
          o.type = 'sine'; o.frequency.value = fr;
          g.gain.setValueAtTime(0.0001, now); g.gain.exponentialRampToValueAtTime(0.05, now + 0.01); g.gain.exponentialRampToValueAtTime(0.0001, now + 0.24);
          o.connect(g); g.connect(s.ac.destination); o.start(now); o.stop(now + 0.26);
        }
      }

      // Sends a message along `path`, a list of node ids. One that meets a device that's away
      // stops there and fails.
      function send(path, color, opts) {
        opts = opts || {};
        let cut = path.findIndex((id, i) => i > 0 && s.d.byId[id] && s.d.byId[id].offline);
        const failed = cut > 0;
        if (failed) path = path.slice(0, cut + 1);
        if (path.length < 2 || !live()) { if (opts.onArrive && !failed) opts.onArrive(); return; }
        const done = () => {
          const end = path[path.length - 1], atHub = s.d.byId[end] && s.d.byId[end].kind === 'hub';
          if (atHub) s.hubPulse = Math.min(1, s.hubPulse + 0.45);
          blip(failed ? 'fail' : opts.note ?? 0, atHub);
          if (failed) { ripple(end, s.theme.poor, 30, 700); burst(end, s.theme.poor, 14, 1.3); }
          else { burst(end, color, opts.size > 3 ? 12 : 6); if (opts.onArrive) opts.onArrive(); }
        };
        s.packets.push({ path, seg: 0, t: 0, color, rgb: rgbOf(color), size: opts.size || 3, trail: [], done });
        s.twang[ekey(path[0], path[1])] = { amp: 9, t: s.t };
        wake();
      }

      // A device's state arrived: a spark from it to Irori. One a command caused goes out from
      // Irori first and comes back.
      function report(id, commanded) {
        const n = s.d && s.d.byId[id];
        if (!n || !live()) return;
        s.heardAt = performance.now();
        const net = netOf(n), color = toneOf(net), up = routeOf(id);
        if (commanded) {
          send(up.slice().reverse(), s.theme.cmd, {
            size: 3.4, note: 'cmd',
            onArrive: () => {
              ripple(id, s.theme.litc, 30, 800); burst(id, s.theme.litc, 16, 1.4); shock(id, 70, 170);
              later(() => send(up, color, { size: 2.8, note: n.net }), 120);
            },
          });
        } else {
          if (n.kind === 'sleepy') ripple(id, color, 22, 700, 0.9);
          burst(id, color, 8);
          send(up, color, { size: 3.2, note: n.net, onArrive: () => { s.hubPulse = Math.min(1, s.hubPulse + 0.3); } });
        }
      }

      // A scan: a sweep round each coordinator and access point, and a hello out to every
      // device, each lighting as it's reached.
      function scan() {
        if (!s.d || !live()) return;
        const now = performance.now();
        s.d.nodes.filter((n) => n.kind === 'coord').forEach((n, i) => {
          const c = toneOf(netOf(n));
          s.sweeps.push({ id: n.id, color: c, rgb: rgbOf(c), t0: now + i * 120, dur: 2100, a0: -Math.PI / 2 + i });
          ripple(n.id, c, 300, 1600, 0.4);
        });
        s.d.nodes.filter((n) => n.kind !== 'hub' && n.kind !== 'coord').forEach((n, i) => {
          later(() => send(routeOf(n.id).reverse(), s.theme.ink, { size: 2, note: 'hello', onArrive: () => ripple(n.id, s.theme.ink, 14, 500, 0.6) }), 120 + i * 70);
        });
        wake();
      }

      function buildCov() {
        const CW = 100, CH = 64;
        const c = s.cov || document.createElement('canvas');
        c.width = CW; c.height = CH; s.cov = c;
        const cx = c.getContext('2d'), img = cx.createImageData(CW, CH);
        const tx = s.d.nodes.filter((n) => (n.kind === 'router' || n.kind === 'coord') && !n.offline && visibleNet(n));
        const poor = s.theme.rgb.poor;
        for (let y = 0; y < CH; y++) for (let x = 0; x < CW; x++) {
          const px = x * 10 + 5, py = y * 10 + 5;
          let best = 0, who = null;
          for (const n of tx) {
            const p = s.pos[n.id], st = Math.exp(-Math.hypot(px - p.x, py - p.y) / (n.kind === 'coord' ? 95 : 80));
            if (st > best) { best = st; who = n; }
          }
          const i = (y * CW + x) * 4;
          if (best < 0.12) { img.data[i] = poor[0]; img.data[i + 1] = poor[1]; img.data[i + 2] = poor[2]; img.data[i + 3] = Math.round(((0.12 - best) / 0.12) * 46); }
          else { const col = toneRgbOf(netOf(who)); img.data[i] = col[0]; img.data[i + 1] = col[1]; img.data[i + 2] = col[2]; img.data[i + 3] = Math.round(Math.min(1, best) * 120); }
        }
        cx.putImageData(img, 0, 0);
        s.covKey = String(s.filter);
      }

      const visibleNet = (n) => s.filter == null || n.kind === 'hub' || (n.nets || [n.net]).includes(s.filter);

      function step(dt) {
        s.t += dt;
        const d = s.d;
        // Between real messages, the mesh talks among itself: a device now and then, a sleepy
        // one waking, a router's beat. Only in a quiet spell: a home that's talking shows that.
        if (s.idle && live() && performance.now() - (s.heardAt || 0) > 4000) {
          s.idleAcc += dt;
          if (s.idleAcc > 0.7) {
            s.idleAcc = 0;
            const pool = d.nodes.filter((n) => !n.offline && n.parent && n.kind !== 'coord' && visibleNet(n));
            const n = pool[Math.floor(Math.random() * pool.length)];
            if (n) {
              const c = toneOf(netOf(n));
              if (n.kind === 'sleepy') { ripple(n.id, c, 22, 700, 0.9); burst(n.id, c, 5, 0.6); }
              send(routeOf(n.id), c, { size: 2.6, note: n.net });
            }
          }
          s.beatAcc += dt;
          if (s.beatAcc > 1.8) {
            s.beatAcc = 0;
            const rs = d.nodes.filter((n) => n.kind === 'router' && !n.offline && visibleNet(n));
            const r = rs[Math.floor(Math.random() * rs.length)];
            if (r) ripple(r.id, toneOf(netOf(r)), 44, 1800, 0.3);
          }
        }
        for (const n of d.nodes) s.pop[n.id] = Math.max(0, (s.pop[n.id] || 0) - dt * 2.5);
        s.hubPulse = Math.max(0, s.hubPulse - dt * 1.2);
        for (const p of s.packets) {
          const u = p.path[p.seg], v = p.path[p.seg + 1], A = s.pos[u], B = s.pos[v];
          if (!A || !B) { p.dead = true; continue; }
          const len = Math.max(20, Math.hypot(B.x - A.x, B.y - A.y)), k = ekey(u, v);
          p.t += (dt * 300) / len;
          s.heat[k] = { v: 1, rgb: p.rgb };
          s.hum[k] = Math.min(1, (s.hum[k] || 0) + dt * 0.9);
          if (p.t >= 1) {
            p.seg++; p.t = 0;
            if (p.seg >= p.path.length - 1) { p.dead = true; p.done(); }
            else {
              const h = p.path[p.seg], hn = d.byId[h];
              if (hn && (hn.kind === 'router' || hn.kind === 'coord')) { ripple(h, p.color, 16, 420, 0.6); s.pop[h] = Math.max(s.pop[h] || 0, 0.5); }
              s.twang[ekey(h, p.path[p.seg + 1])] = { amp: 9, t: s.t };
            }
          }
          if (!p.dead) {
            const a = p.path[p.seg], b = p.path[p.seg + 1], te = p.t * p.t * (3 - 2 * p.t);
            const q = bez(s.pos[a], ctrl(a, b), s.pos[b], te);
            p.x = q.x; p.y = q.y; p.trail.push(q); if (p.trail.length > 22) p.trail.shift();
          }
        }
        s.packets = s.packets.filter((p) => !p.dead);
        const fr = Math.pow(0.03, dt);
        for (const sp of s.sparks) { sp.x += sp.vx * dt; sp.y += sp.vy * dt; sp.vx *= fr; sp.vy *= fr; sp.life -= dt * 1.5; }
        s.sparks = s.sparks.filter((sp) => sp.life > 0);
        for (const k in s.hum) { s.hum[k] -= dt * 0.1; if (s.hum[k] <= 0) delete s.hum[k]; }
        for (const k in s.heat) { s.heat[k].v -= dt * 1.3; if (s.heat[k].v <= 0) delete s.heat[k]; }
      }

      function drawCanvas(ts) {
        const t = s.t, moving = live(), m = s.mouse, th = s.theme, d = s.d;
        ctx.setTransform(scale, 0, 0, scale, 0, 0);
        ctx.globalCompositeOperation = 'source-over';
        ctx.clearRect(0, 0, W, H);
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        if (s.backdrop === 'grid') {
          ctx.fillStyle = th.grid;
          const rs = [];
          for (const r of s.ripples) {
            const k = Math.max(0, (ts - r.t0) / r.dur);
            if (k < 1 && s.pos[r.id]) rs.push({ c: s.pos[r.id], R: 8 + r.maxR * (1 - Math.pow(1 - k, 3)), a: (1 - k) * Math.min(1, r.alpha * 1.6) });
          }
          // Each dot goes in one of a few piles by how bright it is, and each pile is one fill:
          // a dozen draws a frame rather than one per dot.
          const piles = [];
          for (let gx = 11; gx < W; gx += 22) for (let gy = 10; gy < H; gy += 22) {
            let x = gx, y = gy, glow = 0;
            if (moving) for (const r of rs) {
              const dx = x - r.c.x, dy = y - r.c.y, dd = Math.hypot(dx, dy) || 1, w = Math.exp(-Math.pow((dd - r.R) / 16, 2)) * r.a;
              if (w > 0.01) { x += (dx / dd) * w * 10; y += (dy / dd) * w * 10; glow += w; }
            }
            if (m && moving) {
              const dx = m.x - x, dy = m.y - y, dd = Math.hypot(dx, dy);
              if (dd < 150 && dd > 1) { const fo = Math.pow(1 - dd / 150, 2) * 16; x += (dx / dd) * fo; y += (dy / dd) * fo; glow += (1 - dd / 150) * 0.45; }
            }
            const pile = Math.min(12, Math.round(glow * 10));
            (piles[pile] || (piles[pile] = [])).push(x, y);
          }
          piles.forEach((dots, pile) => {
            const glow = pile / 10, rad = 0.9 + glow * 0.9;
            ctx.globalAlpha = Math.min(0.8, 0.12 + glow * 0.6);
            ctx.beginPath();
            for (let i = 0; i < dots.length; i += 2) { ctx.moveTo(dots[i] + rad, dots[i + 1]); ctx.arc(dots[i], dots[i + 1], rad, 0, TAU); }
            ctx.fill();
          });
        } else {
          s.covTick++;
          if (!s.cov || s.tween || s.covKey !== String(s.filter) || s.covTick % 30 === 0) buildCov();
          ctx.globalAlpha = 0.85 + (moving ? Math.sin(t * 1.1) * 0.15 : 0);
          ctx.imageSmoothingEnabled = true;
          ctx.drawImage(s.cov, 0, 0, W, H);
        }
        ctx.globalCompositeOperation = th.blend === 'source-over' ? 'source-over' : 'lighter';
        ctx.globalAlpha = 1;
        if (m && moving) {
          const g = ctx.createRadialGradient(m.x, m.y, 0, m.x, m.y, 170);
          g.addColorStop(0, rgba(th.rgb.glow, 0.09)); g.addColorStop(1, rgba(th.rgb.glow, 0));
          ctx.fillStyle = g; ctx.fillRect(m.x - 170, m.y - 170, 340, 340);
        }
        const hub = d.nodes.find((n) => n.kind === 'hub');
        if (hub) {
          const Hp = s.pos[hub.id], hp = s.hubPulse;
          const r = 70 + hp * 50 + (moving ? Math.sin(t * 1.6) * 5 : 0), g = ctx.createRadialGradient(Hp.x, Hp.y, 0, Hp.x, Hp.y, r);
          g.addColorStop(0, rgba(th.rgb.ink, 0.09 + hp * 0.25)); g.addColorStop(1, rgba(th.rgb.ink, 0));
          ctx.fillStyle = g; ctx.beginPath(); ctx.arc(Hp.x, Hp.y, r, 0, TAU); ctx.fill();
          // A ring round Irori for each network it runs, turning.
          const dashes = [[2, 7], [16, 10], [1, 11], [6, 6]], speeds = [0.6, -0.35, 0.22, -0.5];
          d.networks.slice(0, 4).forEach((net, i) => {
            const rr = 33 + i * 10;
            ctx.save(); ctx.translate(Hp.x, Hp.y); ctx.rotate(moving ? t * speeds[i] : 0);
            ctx.setLineDash(dashes[i]); ctx.strokeStyle = toneOf(net); ctx.globalAlpha = 0.4 + hp * 0.5; ctx.lineWidth = 1.3;
            ctx.beginPath(); ctx.ellipse(0, 0, rr + hp * 10, rr * 0.88 + hp * 10, 0, 0, TAU); ctx.stroke(); ctx.restore();
          });
          ctx.setLineDash([]);
        }
        s.sweeps = s.sweeps.filter((sw) => {
          const k = (ts - sw.t0) / sw.dur;
          if (k >= 1) return false;
          if (k < 0 || !s.pos[sw.id]) return true;
          const P = s.pos[sw.id], ang = sw.a0 + k * TAU * 1.25, fade = 1 - k;
          if (ctx.createConicGradient) {
            const g = ctx.createConicGradient(ang - 1, P.x, P.y);
            g.addColorStop(0, rgba(sw.rgb, 0)); g.addColorStop(1 / TAU, rgba(sw.rgb, 0.32 * fade));
            g.addColorStop(1 / TAU + 0.003, rgba(sw.rgb, 0)); g.addColorStop(1, rgba(sw.rgb, 0));
            ctx.fillStyle = g; ctx.globalAlpha = 1; ctx.beginPath(); ctx.arc(P.x, P.y, 640, 0, TAU); ctx.fill();
          }
          ctx.strokeStyle = sw.color; ctx.globalAlpha = 0.7 * fade; ctx.lineWidth = 1.4;
          ctx.beginPath(); ctx.moveTo(P.x, P.y); ctx.lineTo(P.x + Math.cos(ang) * 640, P.y + Math.sin(ang) * 640); ctx.stroke();
          return true;
        });
        // Each link flows toward Irori, faster on a wire, sparser and stuttering on a weak one.
        if (moving) d.links.forEach((e, i) => {
          const k = ekey(e.a, e.b), vis = s.vis[k] ?? 0.5;
          if (vis < 0.2) return;
          const L = edgeLook(e);
          if (L.word === 'Offline' || L.word === 'Unknown') return;
          const B = d.byId[e.b], up = B && B.parent === e.a, u = up ? e.b : e.a, v = up ? e.a : e.b;
          const A = s.pos[u], Bp = s.pos[v], C = ctrl(u, v), len = Math.hypot(Bp.x - A.x, Bp.y - A.y) || 1;
          const count = 1 + Math.round(L.q * 3), shaky = L.word === 'Weak' || L.word === 'Poor';
          ctx.fillStyle = e.look === 'wired' ? th.ink : L.stroke;
          for (let j = 0; j < count; j++) {
            if (shaky && Math.random() < 0.4) continue;
            const f = ((t * (e.look === 'wired' ? 80 : 40)) / len + j / count + i * 0.37) % 1, p = bez(A, C, Bp, f);
            ctx.globalAlpha = 0.6 * vis * Math.sin(f * Math.PI);
            ctx.beginPath(); ctx.arc(p.x, p.y, 1.5, 0, TAU); ctx.fill();
          }
        });
        for (const k in s.heat) {
          const [a, b] = k.split('|'), A = s.pos[a], B = s.pos[b];
          if (!A || !B) continue;
          const C = ctrl(a, b), h = s.heat[k];
          ctx.strokeStyle = rgba(h.rgb, 1);
          ctx.beginPath(); ctx.moveTo(A.x, A.y); ctx.quadraticCurveTo(C.x, C.y, B.x, B.y);
          ctx.globalAlpha = h.v * 0.35; ctx.lineWidth = 7; ctx.stroke();
          ctx.globalAlpha = h.v * 0.8; ctx.lineWidth = 1.6; ctx.stroke();
        }
        s.ripples = s.ripples.filter((r) => {
          const k = Math.max(0, (ts - r.t0) / r.dur);
          if (k >= 1 || !s.pos[r.id]) return false;
          const c = s.pos[r.id], e = 1 - Math.pow(1 - k, 3);
          ctx.globalAlpha = (1 - k) * r.alpha; ctx.strokeStyle = r.color; ctx.lineWidth = 1.8 * (1 - k) + 0.4;
          ctx.beginPath(); ctx.arc(c.x, c.y, Math.max(0, 8 + r.maxR * e), 0, TAU); ctx.stroke();
          return true;
        });
        for (const p of s.packets) {
          if (p.x == null) continue;
          const tr = p.trail, n = tr.length;
          for (let i = 1; i < n; i++) {
            const f = i / n;
            ctx.strokeStyle = p.color; ctx.globalAlpha = f * 0.75; ctx.lineWidth = p.size * 2 * f;
            ctx.beginPath(); ctx.moveTo(tr[i - 1].x, tr[i - 1].y); ctx.lineTo(tr[i].x, tr[i].y); ctx.stroke();
          }
          const g = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, p.size * 6);
          g.addColorStop(0, rgba(p.rgb, 0.9)); g.addColorStop(1, rgba(p.rgb, 0));
          ctx.globalAlpha = 1; ctx.fillStyle = g; ctx.beginPath(); ctx.arc(p.x, p.y, p.size * 6, 0, TAU); ctx.fill();
          ctx.fillStyle = th.core; ctx.beginPath(); ctx.arc(p.x, p.y, p.size * 0.75, 0, TAU); ctx.fill();
        }
        for (const sp of s.sparks) {
          ctx.fillStyle = sp.c; ctx.globalAlpha = sp.life;
          ctx.beginPath(); ctx.arc(sp.x, sp.y, 1.7 * sp.life + 0.4, 0, TAU); ctx.fill();
        }
        ctx.globalAlpha = 1; ctx.globalCompositeOperation = 'source-over';
      }

      // The SVG: links bent as they are now, devices where they are, dimmed by what's pointed
      // at, filtered out or searched past.
      function drawScene() {
        const d = s.d, th = s.theme, focus = s.hover, q = s.query.trim().toLowerCase();
        const near = new Set();
        if (focus) { near.add(focus); for (const e of d.links) { if (e.a === focus) near.add(e.b); if (e.b === focus) near.add(e.a); } }
        const route = s.sel ? routeOf(s.sel) : [];
        const onRoute = new Set();
        for (let i = 0; i < route.length - 1; i++) onRoute.add(ekey(route[i], route[i + 1]));
        const hidden = (n) => !visibleNet(n);
        for (const e of d.links) {
          const k = ekey(e.a, e.b), el = s.edgeEls[k];
          if (!el) continue;
          const a = s.pos[e.a], b = s.pos[e.b], C = ctrl(e.a, e.b), L = edgeLook(e), routed = onRoute.has(k);
          let op = 0.5;
          if (focus && !(e.a === focus || e.b === focus)) op = 0.1;
          if (hidden(d.byId[e.a]) || hidden(d.byId[e.b])) op = 0.05;
          s.vis[k] = op < 0.2 ? op : 1;
          put(el, 'd', 'M' + a.x.toFixed(1) + ' ' + a.y.toFixed(1) + 'Q' + C.x.toFixed(1) + ' ' + C.y.toFixed(1) + ' ' + b.x.toFixed(1) + ' ' + b.y.toFixed(1));
          put(el, 'stroke', routed ? th.ink : L.stroke);
          put(el, 'stroke-width', String(routed ? L.w + 1.5 : L.w));
          put(el, 'stroke-dasharray', L.dash);
          putStyle(el, 'opacity', String(routed ? 1 : op));
          const lab = s.labelEls[k];
          if (lab) {
            const M = bez(a, C, b, 0.5);
            put(lab.g, 'transform', 'translate(' + M.x.toFixed(1) + ' ' + M.y.toFixed(1) + ')');
            putStyle(lab.g, 'display', s.labels && lab.has ? '' : 'none');
            putStyle(lab.g, 'opacity', String(op < 0.3 ? 0.08 : 1));
          }
        }
        for (const n of d.nodes) {
          const el = s.nodeEls[n.id];
          if (!el) continue;
          const p = s.pos[n.id], match = q && n.name.toLowerCase().includes(q);
          let op = 1;
          if (focus && !near.has(n.id)) op = 0.22;
          if (q && !match) op = Math.min(op, 0.3);
          if (hidden(n)) op = 0.06;
          // The device swells as a message leaves it; its name holds still, to stay readable.
          const k = 1 + (s.pop[n.id] || 0) * 0.45;
          put(el.g, 'transform', 'translate(' + p.x.toFixed(1) + ' ' + p.y.toFixed(1) + ') scale(' + k.toFixed(3) + ')');
          put(el.label, 'transform', 'scale(' + (1 / k).toFixed(3) + ')');
          putStyle(el.g, 'opacity', String(op));
          putStyle(el.g, 'pointerEvents', hidden(n) ? 'none' : 'all');
          el.ring.classList.toggle('on', s.sel === n.id);
          el.label.classList.toggle('found', !!match);
        }
        ringsG.style.opacity = s.layout === 'rings' ? 1 : 0;
      }

      function paint() {
        if (!s.d || s.gone) return;
        if (live()) { wake(); return; }
        // Still: drawn once, as it is.
        computePos(performance.now());
        drawCanvas(performance.now());
        drawScene();
        s.dirty = false;
      }

      function frame(ts) {
        s.raf = 0;
        if (s.gone || !s.d) return;
        if (!live()) { paint(); return; }
        const dt = Math.min(0.05, (ts - (s.last || ts)) / 1000);
        s.last = ts;
        readPointer();
        step(dt);
        computePos(ts);
        drawCanvas(ts);
        drawScene();
        s.raf = requestAnimationFrame(frame);
      }
      function wake() {
        if (!s.raf && !s.gone && live()) { s.last = performance.now(); s.raf = requestAnimationFrame(frame); }
      }
      const onVisibility = () => { if (document.hidden) { cancelAnimationFrame(s.raf); s.raf = 0; } else paint(); };
      document.addEventListener('visibilitychange', onVisibility);

      // The pointer is only noted as it moves, and placed on the map once a frame: measuring the
      // map on every move, while each frame rewrites it, made the browser lay the page out again
      // for every move.
      root.addEventListener('mousemove', (event) => { s.client = { x: event.clientX, y: event.clientY }; });
      root.addEventListener('mouseleave', () => { s.client = null; s.mouse = null; });
      function readPointer() {
        if (!s.client) { s.mouse = null; return; }
        const r = root.getBoundingClientRect();
        s.mouse = { x: ((s.client.x - r.left) * W) / r.width, y: ((s.client.y - r.top) * H) / r.height };
      }

      // Dragging a device: it goes where it's put and stays there, in this layout, in this
      // browser. A press that doesn't travel a few pixels is still a click.
      function startDrag(event, id) {
        if (event.button !== 0 || !s.pos[id]) return;
        s.drag = { id, pointer: event.pointerId, x0: event.clientX, y0: event.clientY, rect: root.getBoundingClientRect(), from: { x: s.pos[id].x, y: s.pos[id].y }, moved: false };
      }
      function onDragMove(event) {
        const d = s.drag;
        if (!d || event.pointerId !== d.pointer) return;
        if (!d.moved && Math.hypot(event.clientX - d.x0, event.clientY - d.y0) < 4) return;
        if (!d.moved) { d.moved = true; root.classList.add('nm-dragging'); }
        event.preventDefault();
        const x = d.from.x + ((event.clientX - d.x0) * W) / d.rect.width, y = d.from.y + ((event.clientY - d.y0) * H) / d.rect.height;
        placesOf()[d.id] = { x: Math.round(Math.max(16, Math.min(W - 16, x))), y: Math.round(Math.max(16, Math.min(H - 28, y))) };
        s.tween = null; s.cov = null; s.dirty = true;
        paint();
      }
      function onDragEnd(event) {
        const d = s.drag;
        if (!d || event.pointerId !== d.pointer) return;
        s.drag = null;
        if (!d.moved) return;
        root.classList.remove('nm-dragging');
        s.dragged = true;
        // Let go off the device, no click follows to clear this.
        later(() => { s.dragged = false; }, 0);
        savePlaces();
        arranged();
      }
      window.addEventListener('pointermove', onDragMove);
      window.addEventListener('pointerup', onDragEnd);
      window.addEventListener('pointercancel', onDragEnd);

      return {
        setDrawing(json) { try { setDrawing(JSON.parse(json)); } catch (error) { console.error('network map', error); } },
        select(id) { s.sel = id || null; s.dirty = true; paint(); },
        setFilter(index) { s.filter = index == null || index < 0 ? null : index; s.cov = null; paint(); },
        setQuery(query) { s.query = query || ''; paint(); },
        setLabels(on) { s.labels = !!on; paint(); },
        setLayout(layout) {
          if (layout === s.layout || !s.layouts) { s.layout = layout; paint(); return; }
          s.tween = { from: JSON.parse(JSON.stringify(s.base)), t0: performance.now(), dur: live() ? 650 : 0 };
          s.layout = layout; s.cov = null;
          arranged();
          paint();
        },
        resetLayout() {
          s.tween = { from: JSON.parse(JSON.stringify(s.base)), t0: performance.now(), dur: live() ? 650 : 0 };
          s.places[s.layout] = {};
          savePlaces();
          s.cov = null; s.dirty = true;
          arranged();
          paint();
        },
        setBackdrop(backdrop) { s.backdrop = backdrop === 'coverage' ? 'coverage' : 'grid'; s.cov = null; paint(); },
        setSound(on) {
          s.sound = !!on;
          if (s.sound && !s.ac) { const AC = window.AudioContext || window.webkitAudioContext; if (AC) s.ac = new AC(); }
          if (s.ac && s.sound) s.ac.resume();
        },
        setMotion(on) { s.motion = !!on; if (!s.motion) { s.packets = []; s.sparks = []; s.ripples = []; s.sweeps = []; s.tween = null; } paint(); },
        setActive(on) { s.active = !!on; if (!s.active) { cancelAnimationFrame(s.raf); s.raf = 0; } paint(); },
        report(id, commanded) { report(id, !!commanded); },
        scan() { scan(); },
        destroy() {
          s.gone = true;
          cancelAnimationFrame(s.raf);
          s.timers.forEach(clearTimeout);
          document.removeEventListener('visibilitychange', onVisibility);
          window.removeEventListener('pointermove', onDragMove);
          window.removeEventListener('pointerup', onDragEnd);
          window.removeEventListener('pointercancel', onDragEnd);
          if (resized) resized.disconnect();
          if (scheme && scheme.removeEventListener) scheme.removeEventListener('change', onScheme);
          if (s.ac) s.ac.close();
          root.textContent = '';
        },
      };
    },
  };
})();
