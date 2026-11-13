"""Checks the web client's handling of a microphone with one empty channel
(as a Mac's built-in microphone arrives: two channels, the right one
silent), in headless Chrome: the `micChannels` part of web/app.js, taken
from the file as it is, runs on Chrome's fake microphone, and what it
outputs is measured per channel.

1. A one-live-channel microphone comes out in both channels, at the level
   it was recorded, and the switch is reported ("left").
2. A real stereo microphone (440 Hz left, 1000 Hz right) stays stereo.

Environment: SELENIUM (a Selenium server whose Chrome can reach this
machine's 127.0.0.1, e.g. selenium/standalone-chrome with --network host),
LEFT_ONLY and STEREO (WAV paths where that Chrome runs). The WAV files:
LEFT_ONLY 440 Hz left and silence right, STEREO 440 Hz left and 1000 Hz
right, both at amplitude 0.25 (RMS 0.177), 48 kHz.
Requires: pip install selenium.
"""
import http.server, os, re, socketserver, sys, threading

from selenium import webdriver

HERE = os.path.dirname(os.path.abspath(__file__))
APP = os.path.join(HERE, "..", "..", "web", "app.js")
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'  (' + str(detail) + ')' if detail and not ok else ''}", flush=True)


code = re.search(r"// mic-channels:begin.*?\n(.*)// mic-channels:end", open(APP).read(), re.S).group(1)
page = """<!doctype html><meta charset="utf-8"><title>mic channels</title><script>
""" + code + """
async function measure() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: { ideal: 2 } } });
  const ctx = new AudioContext();
  // In the app this follows a click (Send my microphone), so it runs; here
  // it's started explicitly.
  await ctx.resume();
  const source = ctx.createMediaStreamSource(stream);
  // Chrome's fake microphone can take a moment to start: wait for sound.
  const probe = ctx.createAnalyser();
  probe.fftSize = 1024;
  source.connect(probe);
  const pb = new Float32Array(1024);
  let heard = false;
  for (let i = 0; i < 100 && !heard; i++) {
    probe.getFloatTimeDomainData(pb);
    heard = pb.some((x) => Math.abs(x) > 1e-3);
    if (!heard) await new Promise((r) => setTimeout(r, 50));
  }
  probe.disconnect();
  if (!heard) return { error: "the test microphone delivered no sound in 5 seconds" };
  let switched = null;
  const ch = micChannels(ctx, source, (live) => (switched = live));
  const out = ctx.createMediaStreamDestination();
  out.channelCount = 2;
  out.channelCountMode = "explicit";
  ch.output.connect(out);
  // Measure what's sent: the destination's stream, per channel.
  const back = ctx.createMediaStreamSource(out.stream);
  const split = ctx.createChannelSplitter(2);
  back.connect(split);
  const an = [0, 1].map((i) => { const a = ctx.createAnalyser(); a.fftSize = 8192; a.smoothingTimeConstant = 0; split.connect(a, i); return a; });
  await new Promise((r) => setTimeout(r, 2000));
  const res = an.map((a) => {
    const f = new Float32Array(a.frequencyBinCount); a.getFloatFrequencyData(f);
    let bi = 1; for (let i = 2; i < f.length; i++) if (f[i] > f[bi]) bi = i;
    const td = new Float32Array(a.fftSize); a.getFloatTimeDomainData(td);
    let s = 0; for (const v of td) s += v * v;
    return { freq: bi * ctx.sampleRate / a.fftSize, rms: Math.sqrt(s / td.length) };
  });
  ch.stop();
  return { state: ctx.state, channels: stream.getAudioTracks()[0].getSettings().channelCount, switched, left: res[0], right: res[1] };
}
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = page.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


httpd = socketserver.TCPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=httpd.serve_forever, daemon=True).start()
url = f"http://127.0.0.1:{httpd.server_address[1]}/"


def run(wav):
    o = webdriver.ChromeOptions()
    for a in ("--headless=new", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
              f"--use-file-for-fake-audio-capture={wav}", "--autoplay-policy=no-user-gesture-required"):
        o.add_argument(a)
    d = webdriver.Remote(command_executor=os.environ.get("SELENIUM", "http://127.0.0.1:4444"), options=o)
    try:
        d.get(url)
        d.set_script_timeout(20)
        return d.execute_async_script("measure().then(arguments[0], (e) => arguments[0]({ error: String(e) }))")
    finally:
        d.quit()


r = run(os.environ.get("LEFT_ONLY", "/tmp/left-only-tone.wav"))
print("one live channel:", r)
check("a one-live-channel microphone comes out in both channels", "error" not in r
      and all(abs(r[c]["freq"] - 440) < 15 and 0.12 < r[c]["rms"] < 0.24 for c in ("left", "right")), r)
check("at the same level", "error" not in r and abs(r["left"]["rms"] - r["right"]["rms"]) < 0.01, r)
check("and the switch is reported as the left channel", r.get("switched") == "left", r)

r = run(os.environ.get("STEREO", "/tmp/stereo-tone.wav"))
print("stereo:", r)
check("a stereo microphone stays stereo (440 Hz left, 1000 Hz right)", "error" not in r
      and abs(r["left"]["freq"] - 440) < 15 and abs(r["right"]["freq"] - 1000) < 15, r)
check("with nothing switched", r.get("switched") is None, r)

httpd.shutdown()
print(f"\n{sum(results)}/{len(results)} checks passed")
sys.exit(0 if all(results) else 1)
