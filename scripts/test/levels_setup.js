const s = [...state.sessions.values()][0];
const ctx = new AudioContext();
const src = ctx.createMediaStreamSource(s.audio.srcObject);
const sp = ctx.createChannelSplitter(2);
src.connect(sp);
const l = ctx.createAnalyser(), r = ctx.createAnalyser();
l.fftSize = 8192; r.fftSize = 8192;
sp.connect(l, 0); sp.connect(r, 1);
window.__l = l; window.__r = r; window.__src = src;
