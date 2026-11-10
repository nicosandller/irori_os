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
// in full screen with its controls.
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

  window.iroriCameraLive = { mount, accepts, full };
})();
