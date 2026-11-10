// A camera's live video (camera_live.rs): the socket at /api/entities/<id>/live, played through
// Media Source Extensions (`docs/specs/protocols.md` §5.3).
//
// The page says which codecs it can play, the server says what the stream is ("playing", with
// its MIME type), then fragmented MP4 arrives as binary frames: the init segment, then one
// fragment per frame. "ended" says why it stopped.
//
// Kept short and at the live edge: a few seconds of buffer, a jump forward when it falls
// behind, and a jump over any hole (a page that joins late, or a frame dropped to keep up,
// leaves one). Closed while the tab is hidden, and opened again when it's back. `full` puts it
// in full screen with its controls. `magnify` shows a wide camera a slice at a time, with the
// whole picture in a strip to slide the slice along.
(function () {
  // What a page can play, by the codec family the camera's stream names.
  const PROBES = [
    ['avc1', 'video/mp4; codecs="avc1.42E01E"'],
    ['hvc1', 'video/mp4; codecs="hvc1.1.6.L93.B0"'],
    ['mp4a', 'audio/mp4; codecs="mp4a.40.2"'],
    ['flac', 'audio/mp4; codecs="flac"'],
  ];
  // How far behind the live edge playback may fall before it jumps forward, in seconds.
  const BEHIND = 2.5;
  // How much is kept behind what's playing.
  const KEEP = 8;
  // The most fragments waiting to be appended: past this the page can't keep up.
  const MAX_QUEUE = 120;

  function Source() {
    return window.ManagedMediaSource || window.MediaSource || null;
  }

  function accepts() {
    const M = Source();
    if (!M) return [];
    return PROBES.filter(([, type]) => {
      try {
        return M.isTypeSupported(type);
      } catch (_) {
        return false;
      }
    }).map(([family]) => family);
  }

  // Plays `path` (e.g. /api/entities/camera.door/live?quality=low) in `video`, and calls
  // `onState(state, reason)` with "starting", "playing" or "ended". Returns { close }.
  function mount(video, path, onState) {
    let socket = null;
    let source = null;
    let buffer = null;
    let queue = [];
    let closed = false;
    let ended = false;
    let timer = null;

    function say(state, reason) {
      try {
        onState(state, reason || '');
      } catch (_) {}
    }

    function stop(reason) {
      if (ended) return;
      ended = true;
      if (socket) {
        socket.onclose = null;
        try {
          socket.close();
        } catch (_) {}
      }
      socket = null;
      if (timer) clearInterval(timer);
      timer = null;
      queue = [];
      if (reason !== null) say('ended', reason);
    }

    function feed() {
      if (!buffer || buffer.updating || queue.length === 0) return;
      try {
        buffer.appendBuffer(queue.shift());
      } catch (error) {
        if (error && error.name === 'QuotaExceededError') {
          trim(1);
          return;
        }
        stop("this browser couldn't play the camera's video");
      }
    }

    // Drops what's long played, keeping `keep` seconds behind the playhead.
    function trim(keep) {
      if (!buffer || buffer.updating || buffer.buffered.length === 0) return;
      const start = buffer.buffered.start(0);
      const until = video.currentTime - keep;
      if (until > start + 0.5) {
        try {
          buffer.remove(start, until);
        } catch (_) {}
      }
    }

    // Keeps playback inside what's buffered and near its end.
    function watch() {
      if (!buffer || buffer.buffered.length === 0) return;
      const ranges = buffer.buffered;
      const last = ranges.end(ranges.length - 1);
      const now = video.currentTime;
      let inside = false;
      for (let i = 0; i < ranges.length; i++) {
        if (now >= ranges.start(i) && now < ranges.end(i)) inside = true;
      }
      if (!inside || last - now > BEHIND) {
        // Into the newest range, just short of its end.
        const start = ranges.start(ranges.length - 1);
        video.currentTime = Math.max(start, last - 0.3);
      }
      if (video.paused) video.play().catch(() => {});
      trim(KEEP);
    }

    function begin(mime) {
      const M = Source();
      if (!M || !M.isTypeSupported(mime)) {
        stop("this browser can't play this camera's video");
        return;
      }
      source = new M();
      // Safari's ManagedMediaSource only plays with AirPlay switched off.
      if (window.ManagedMediaSource && source instanceof window.ManagedMediaSource) {
        video.disableRemotePlayback = true;
      }
      video.muted = true;
      video.playsInline = true;
      video.src = URL.createObjectURL(source);
      source.addEventListener('sourceopen', () => {
        try {
          buffer = source.addSourceBuffer(mime);
          buffer.mode = 'segments';
        } catch (_) {
          stop("this browser can't play this camera's video");
          return;
        }
        buffer.addEventListener('updateend', feed);
        feed();
      });
      // Whether it has sound, so the page offers to turn it on.
      const sound = /mp4a|flac/.test(mime);
      video.addEventListener(
        'playing',
        () => {
          if (!ended) say('playing', sound ? 'sound' : '');
        },
        { once: true },
      );
      timer = setInterval(watch, 500);
    }

    function open() {
      ended = false;
      queue = [];
      buffer = null;
      say('starting');
      const scheme = location.protocol === 'https:' ? 'wss://' : 'ws://';
      socket = new WebSocket(scheme + location.host + path);
      socket.binaryType = 'arraybuffer';
      socket.onopen = () => socket.send(JSON.stringify({ accepts: accepts() }));
      socket.onmessage = (event) => {
        if (typeof event.data === 'string') {
          let word = null;
          try {
            word = JSON.parse(event.data);
          } catch (_) {
            return;
          }
          if (word.playing) begin(word.playing.mime);
          if (word.ended) stop(word.ended.reason || 'the stream ended');
          return;
        }
        if (queue.length >= MAX_QUEUE) {
          stop("this page couldn't keep up with the camera");
          return;
        }
        queue.push(event.data);
        feed();
      };
      socket.onclose = () => stop('the connection to Irori closed');
    }

    // Nothing is watched from a tab nobody is looking at.
    function visibility() {
      if (closed) return;
      if (document.hidden) {
        stop(null);
        say('paused');
      } else if (ended) {
        reset();
        open();
      }
    }

    function reset() {
      if (source && video.src) {
        try {
          URL.revokeObjectURL(video.src);
        } catch (_) {}
      }
      video.removeAttribute('src');
      source = null;
      buffer = null;
    }

    document.addEventListener('visibilitychange', visibility);
    if (document.hidden) {
      ended = true;
      say('paused');
    } else {
      open();
    }

    return {
      close() {
        if (closed) return;
        closed = true;
        document.removeEventListener('visibilitychange', visibility);
        stop(null);
        reset();
        try {
          video.load();
        } catch (_) {}
      },
    };
  }

  // How long the controls stay over a full-screen video after the pointer last moved, in ms.
  const IDLE_MS = 2600;

  // Puts `stage` (the video with its controls) in full screen, or takes it out. The browser's
  // own full screen where there is one; where there isn't (an iPhone), the stage just covers
  // the page. The controls sit over the video and fade when the pointer is still. `onLeave` is
  // called when it leaves some other way: Escape, or the system's own gesture.
  function full(stage, on, onLeave) {
    const was = stage.__iroriFull;
    if (!on) {
      if (was) was.leave(false);
      return;
    }
    if (was) return;
    let timer = null;
    const wake = () => {
      delete stage.dataset.idle;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        stage.dataset.idle = '';
      }, IDLE_MS);
    };
    const changed = () => {
      const now = document.fullscreenElement || document.webkitFullscreenElement;
      if (!now && state.native) state.leave(true);
    };
    const key = (event) => {
      if (event.key === 'Escape') state.leave(true);
    };
    const state = {
      native: false,
      leave(told) {
        if (stage.__iroriFull !== state) return;
        delete stage.__iroriFull;
        if (timer) clearTimeout(timer);
        stage.classList.remove('is-full');
        delete stage.dataset.idle;
        document.documentElement.classList.remove('camera-full');
        stage.removeEventListener('pointermove', wake);
        stage.removeEventListener('pointerdown', wake);
        document.removeEventListener('fullscreenchange', changed);
        document.removeEventListener('webkitfullscreenchange', changed);
        document.removeEventListener('keydown', key);
        const now = document.fullscreenElement || document.webkitFullscreenElement;
        if (now === stage) {
          const exit = document.exitFullscreen || document.webkitExitFullscreen;
          try {
            const done = exit && exit.call(document);
            if (done && done.catch) done.catch(() => {});
          } catch (_) {}
        }
        if (told) {
          try {
            onLeave();
          } catch (_) {}
        }
      },
    };
    stage.__iroriFull = state;
    stage.classList.add('is-full');
    document.documentElement.classList.add('camera-full');
    stage.addEventListener('pointermove', wake);
    stage.addEventListener('pointerdown', wake);
    document.addEventListener('fullscreenchange', changed);
    document.addEventListener('webkitfullscreenchange', changed);
    document.addEventListener('keydown', key);
    wake();
    const request = stage.requestFullscreen || stage.webkitRequestFullscreen;
    if (request) {
      try {
        const asked = request.call(stage);
        state.native = true;
        if (asked && asked.catch) {
          // Refused: the stage still covers the page.
          asked.catch(() => {
            state.native = false;
          });
        }
      } catch (_) {}
    }
  }

  // The magnifier, for a camera much wider than the screen (a two-lens panorama). The whole
  // picture sits in its frame (`object-fit: contain`); magnified, it's scaled up until it fills
  // the frame's height, so the frame shows a slice, and moved sideways to the slice `pan` asks
  // for (0 = the left edge, 1 = the right). Scale and move are CSS transitions, so turning it
  // on and off is a zoom. `canvas` draws the whole picture with the slice marked. Dragging the
  // strip moves the slice to the pointer, dragging the video moves it the way a photo is
  // dragged, and the strip takes the arrow keys; `range` (a 0–1000 slider, used in full
  // screen) moves it too. `onPan(pan)` is told where it was left. Returns { stop }.
  function magnify(video, canvas, pan, onPan, range) {
    let stopped = false;
    let frame = null;
    pan = Math.min(1, Math.max(0, Number(pan) || 0.5));
    const context = canvas.getContext('2d');
    // The slice's box over the strip: its own layer, moved like the video, so a drag draws
    // nothing. Outside it is dimmed by its shadow, clipped to the strip.
    const box = canvas.parentElement && canvas.parentElement.querySelector('.camera-strip-box');

    // Sizes, measured when something changes size rather than on every move: reading them
    // right after moving the picture would make the browser lay the page out again each time.
    let part = 1;
    function measure() {
      const width = video.videoWidth;
      const height = video.videoHeight;
      const shownWidth = video.clientWidth;
      const shownHeight = video.clientHeight;
      part = !width || !height || !shownHeight
        ? 1
        : Math.min(1, (shownWidth / shownHeight) / (width / height));
      if (box) box.style.width = (part * 100).toFixed(3) + '%';
      placed = '';
    }

    // The scale and move for the slice; percentages of the frame's width, so they hold as
    // the frame changes size.
    let placed = '';
    function place() {
      const grow = 1 / part;
      const move = ((0.5 - pan) * (grow - 1) * 100).toFixed(3) + '% 0';
      const now = grow.toFixed(4) + '|' + move;
      if (now === placed) return;
      placed = now;
      video.style.scale = grow.toFixed(4);
      video.style.translate = move;
      // The box's move is in its own widths: the slice's left edge over the strip.
      if (box) {
        box.style.translate = part < 1 ? ((pan * (1 - part) / part) * 100).toFixed(3) + '% 0' : '0 0';
      }
      canvas.setAttribute('aria-valuenow', String(Math.round(pan * 100)));
      if (range) range.value = String(Math.round(pan * 1000));
    }

    // The whole picture in the strip: a copy of a camera frame, which costs, so a few times a
    // second at most, and not while the slice is being dragged.
    let painted = 0;
    let dragging = false;
    function paint(force) {
      if (stopped) return;
      const now = performance.now();
      if (!force && (dragging || now - painted < 125)) return;
      const width = video.videoWidth;
      const height = video.videoHeight;
      const shown = canvas.clientWidth;
      if (!width || !height || !shown) return;
      painted = now;
      const ratio = window.devicePixelRatio || 1;
      const w = Math.round(shown * ratio);
      const h = Math.round((shown * height / width) * ratio);
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
        canvas.style.aspectRatio = width + ' / ' + height;
      }
      context.drawImage(video, 0, 0, w, h);
    }

    function draw() {
      paint(false);
      next();
    }

    function next() {
      if (stopped) return;
      // A new frame when the video has one; otherwise each screen refresh.
      if (video.requestVideoFrameCallback) {
        frame = { video: video.requestVideoFrameCallback(draw) };
      } else {
        frame = { animation: requestAnimationFrame(draw) };
      }
    }

    // Everything moves at once: the picture and the box are only restyled.
    function set(to) {
      pan = Math.min(1, Math.max(0, to));
      place();
    }

    function resize() {
      measure();
      place();
      paint(true);
    }

    // The strip: the slice centres on the pointer.
    // Where the strip is, measured once when the drag starts.
    let stripDrag = null;
    function toPointer(event) {
      if (!stripDrag || part >= 1 || !stripDrag.width) return;
      const centre = (event.clientX - stripDrag.left) / stripDrag.width;
      set((centre - part / 2) / (1 - part));
    }
    function stripDown(event) {
      const at = canvas.getBoundingClientRect();
      stripDrag = { left: at.left, width: at.width };
      dragging = true;
      canvas.setPointerCapture(event.pointerId);
      video.classList.add('dragging');
      if (box) box.classList.add('dragging');
      toPointer(event);
      event.preventDefault();
    }
    function stripMove(event) {
      toPointer(event);
    }
    function stripUp() {
      if (!stripDrag) return;
      stripDrag = null;
      dragging = false;
      video.classList.remove('dragging');
      if (box) box.classList.remove('dragging');
      onPan(pan);
    }

    // The video: dragged like a photo, so the slice moves the other way.
    let videoDrag = null;
    function videoDown(event) {
      if (event.button !== 0) return;
      videoDrag = { x: event.clientX, pan, spare: video.clientWidth * (1 / part - 1) };
      dragging = true;
      video.setPointerCapture(event.pointerId);
      video.classList.add('dragging');
    }
    function videoMove(event) {
      if (!videoDrag) return;
      if (videoDrag.spare > 0) set(videoDrag.pan - (event.clientX - videoDrag.x) / videoDrag.spare);
    }
    function videoUp() {
      if (!videoDrag) return;
      videoDrag = null;
      dragging = false;
      video.classList.remove('dragging');
      onPan(pan);
    }

    function key(event) {
      const step = event.shiftKey ? 0.25 : 0.05;
      const to = { ArrowLeft: pan - step, ArrowRight: pan + step, Home: 0, End: 1 }[event.key];
      if (to === undefined) return;
      event.preventDefault();
      set(to);
      onPan(pan);
    }

    // The slider: held, the picture follows it at once (like the drags); a key press on it
    // still glides.
    function slideDown() {
      dragging = true;
      video.classList.add('dragging');
      // Let go anywhere, even off the slider or without moving it.
      window.addEventListener('pointerup', slideUp, { once: true });
      window.addEventListener('pointercancel', slideUp, { once: true });
    }
    function slideUp() {
      window.removeEventListener('pointerup', slideUp);
      window.removeEventListener('pointercancel', slideUp);
      dragging = false;
      video.classList.remove('dragging');
    }
    function slide() {
      set(Number(range.value) / 1000);
    }
    function slid() {
      onPan(pan);
    }

    // A frame that changes size (full screen, a turned phone) needs a new zoom even when no
    // new picture comes, as when the video has stopped.
    const resized = window.ResizeObserver ? new ResizeObserver(resize) : null;

    canvas.addEventListener('pointerdown', stripDown);
    if (range) {
      range.addEventListener('pointerdown', slideDown);
      range.addEventListener('input', slide);
      range.addEventListener('change', slid);
    }
    if (resized) {
      resized.observe(video);
      resized.observe(canvas);
    }
    video.addEventListener('loadedmetadata', resize);
    video.addEventListener('resize', resize);
    canvas.addEventListener('pointermove', stripMove);
    canvas.addEventListener('pointerup', stripUp);
    canvas.addEventListener('pointercancel', stripUp);
    canvas.addEventListener('keydown', key);
    video.addEventListener('pointerdown', videoDown);
    video.addEventListener('pointermove', videoMove);
    video.addEventListener('pointerup', videoUp);
    video.addEventListener('pointercancel', videoUp);
    measure();
    place();
    next();

    return {
      stop() {
        stopped = true;
        if (frame && frame.video !== undefined && video.cancelVideoFrameCallback) {
          video.cancelVideoFrameCallback(frame.video);
        }
        if (frame && frame.animation !== undefined) cancelAnimationFrame(frame.animation);
        if (range) {
          range.removeEventListener('pointerdown', slideDown);
          range.removeEventListener('input', slide);
          range.removeEventListener('change', slid);
        }
        slideUp();
        if (resized) resized.disconnect();
        canvas.removeEventListener('pointerdown', stripDown);
        canvas.removeEventListener('pointermove', stripMove);
        canvas.removeEventListener('pointerup', stripUp);
        canvas.removeEventListener('pointercancel', stripUp);
        canvas.removeEventListener('keydown', key);
        video.removeEventListener('pointerdown', videoDown);
        video.removeEventListener('pointermove', videoMove);
        video.removeEventListener('pointerup', videoUp);
        video.removeEventListener('pointercancel', videoUp);
        video.removeEventListener('loadedmetadata', resize);
        video.removeEventListener('resize', resize);
        video.classList.remove('dragging');
        video.style.scale = '';
        video.style.translate = '';
      },
    };
  }

  window.iroriCameraLive = { mount, accepts, full, magnify };
})();
