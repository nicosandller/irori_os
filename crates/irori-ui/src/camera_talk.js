// Talking through a camera (camera_talk.rs): the microphone, out of the camera's speaker, over
// the socket at /api/entities/<id>/talk (`docs/specs/protocols.md` §5.4).
//
// While the button is held: the microphone (with the browser's echo cancelling, so the camera's
// own sound isn't sent back to it), two low-pass filters that keep speech and drop what 8 kHz
// can't carry, then camera_talk_worklet.js, which posts 20 ms of 16-bit sound at 8 kHz at a
// time. The socket opens once the microphone runs, and nothing is sent until the server says
// the camera is "listening"; letting go closes it all, the microphone included.
(function () {
  // Speech is below this; 8000 samples a second carry nothing above 4 kHz.
  const LOW_PASS_HZ = 3400;
  // What's still on its way when the button is let go, in milliseconds.
  const FLUSH_MS = 150;

  // A browser only lets a page use the microphone on https or on this computer.
  function possible() {
    return !!(window.isSecureContext && navigator.mediaDevices && navigator.mediaDevices.getUserMedia);
  }

  // Starts talking through `path` (e.g. /api/entities/camera.door/talk), and calls
  // `onState(state, reason)` with "connecting", "talking" or "ended". An ended reason is the
  // server's words, or "insecure", "blocked" or "no-microphone" for what the browser refused, or
  // "" when it was let go. Call from the press itself: a browser starts sound only for a gesture.
  // `levelOn`, when given, gets how loud the voice is as `--level` (0 to 1), for the button to
  // move with it. Returns { stop }.
  function start(path, onState, levelOn) {
    let level = 0;
    function show(chunk) {
      if (!levelOn) return;
      const samples = new Int16Array(chunk);
      let sum = 0;
      for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i];
      const loud = Math.min(1, (Math.sqrt(sum / Math.max(1, samples.length)) / 32768) * 5);
      // Up at once, down slowly: speech reads as a pulse rather than a flicker.
      level = Math.max(loud, level * 0.82);
      levelOn.style.setProperty('--level', level.toFixed(3));
    }
    function quiet() {
      if (levelOn) levelOn.style.setProperty('--level', '0');
    }
    let ended = false;
    let listening = false;
    let socket = null;
    let stream = null;
    let context = null;

    function say(state, reason) {
      try {
        onState(state, reason || '');
      } catch (_) {}
    }

    function release() {
      quiet();
      if (stream) {
        stream.getTracks().forEach((track) => {
          try {
            track.stop();
          } catch (_) {}
        });
      }
      stream = null;
      if (context) {
        try {
          context.close();
        } catch (_) {}
      }
      context = null;
    }

    function closeSocket() {
      if (!socket) return;
      const closing = socket;
      socket = null;
      closing.onclose = null;
      closing.onmessage = null;
      try {
        closing.close();
      } catch (_) {}
    }

    function finish(reason) {
      if (ended) return;
      ended = true;
      release();
      closeSocket();
      say('ended', reason);
    }

    // Said at once, while the caller is still listening.
    if (!possible()) {
      finish('insecure');
      return { stop() {} };
    }

    say('connecting');
    try {
      context = new (window.AudioContext || window.webkitAudioContext)();
    } catch (_) {
      finish('insecure');
      return { stop() {} };
    }

    // Dialled once the microphone is running, so a first-time "allow the microphone?" doesn't
    // eat into the few seconds the camera waits for sound.
    function dial() {
      const scheme = location.protocol === 'https:' ? 'wss://' : 'ws://';
      socket = new WebSocket(scheme + location.host + path);
      socket.binaryType = 'arraybuffer';
      socket.onmessage = (event) => {
        if (typeof event.data !== 'string') return;
        let word = null;
        try {
          word = JSON.parse(event.data);
        } catch (_) {
          return;
        }
        if (word === 'listening') {
          listening = true;
          say('talking');
        } else if (word && word.ended) {
          finish(word.ended.reason || 'the camera stopped listening');
        }
      };
      socket.onclose = () => finish('the connection to Irori closed');
    }

    const microphone = navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
    Promise.all([microphone, context.audioWorklet.addModule('/camera_talk_worklet.js')])
      .then(([got]) => {
        if (ended || !context) {
          got.getTracks().forEach((track) => track.stop());
          return;
        }
        stream = got;
        const source = context.createMediaStreamSource(stream);
        let last = source;
        for (let i = 0; i < 2; i++) {
          const filter = context.createBiquadFilter();
          filter.type = 'lowpass';
          filter.frequency.value = LOW_PASS_HZ;
          last.connect(filter);
          last = filter;
        }
        const node = new AudioWorkletNode(context, 'irori-talk');
        node.port.onmessage = (event) => {
          if (listening && socket && socket.readyState === WebSocket.OPEN) {
            show(event.data);
            socket.send(event.data);
          }
        };
        last.connect(node);
        // A node nothing listens to may never run; it writes nothing, so this is silent.
        node.connect(context.destination);
        if (context.state === 'suspended') context.resume();
        dial();
      })
      .catch((error) => {
        const name = error && error.name;
        if (name === 'NotAllowedError' || name === 'SecurityError') {
          finish('blocked');
        } else if (name === 'NotFoundError' || name === 'OverconstrainedError') {
          finish('no-microphone');
        } else {
          finish((error && error.message) || 'the microphone didn\'t start');
        }
      });

    return {
      // Let go: the microphone stops now, and what's already on its way gets a moment to go.
      stop() {
        if (ended) return;
        ended = true;
        release();
        const closing = socket;
        socket = null;
        if (closing) {
          closing.onclose = null;
          closing.onmessage = null;
          setTimeout(() => {
            try {
              closing.close();
            } catch (_) {}
          }, listening ? FLUSH_MS : 0);
        }
        say('ended', '');
      },
    };
  }

  window.iroriCameraTalk = { start, possible };
})();
