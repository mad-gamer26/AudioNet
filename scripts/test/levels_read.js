const lv = (a) => { const x = new Float32Array(a.fftSize); a.getFloatTimeDomainData(x); let m = 0; for (const v of x) m = Math.max(m, Math.abs(v)); return 20 * Math.log10(m); };
const s = [...state.sessions.values()][0];
return { left_peak_dbfs: lv(window.__l), right_peak_dbfs: lv(window.__r), source_channels: window.__src.channelCount,
         track: s.audio.srcObject.getAudioTracks()[0].getSettings() };
