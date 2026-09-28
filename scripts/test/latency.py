# End-to-end latency measurement with noise bursts.
# Usage: python latency.py OUTPUT_NAME REFERENCE_INPUT_NAME DESTINATION_INPUT_NAME SECONDS
# Plays bursts into OUTPUT_NAME (the AudioNet source side), records
# REFERENCE_INPUT_NAME (the same signal, before AudioNet) and
# DESTINATION_INPUT_NAME (after AudioNet), aligned by PortAudio ADC time, and
# reports per-burst delays. Device names are WASAPI name prefixes.
import sys, time, numpy as np, sounddevice as sd
def find(name, kind):
    w = [i for i,a in enumerate(sd.query_hostapis()) if 'WASAPI' in a['name']][0]
    for i,d in enumerate(sd.query_devices()):
        if d['hostapi']==w and d['name'].startswith(name) and d['max_%s_channels' % kind] > 0: return i
    raise SystemExit('no device ' + name)
OUT, REF, DST = find(sys.argv[1], 'output'), find(sys.argv[2], 'input'), find(sys.argv[3], 'input')
secs = float(sys.argv[4]); sr = 48000
chunks = {'ref': [], 'dst': []}
def cb(key):
    def f(indata, frames, t, status):
        chunks[key].append((t.inputBufferAdcTime, indata[:, 0].copy()))
    return f
streams = [sd.InputStream(device=d, samplerate=sr, channels=2, callback=cb(k)) for k, d in (('ref', REF), ('dst', DST))]
for s in streams: s.start()
rng = np.random.default_rng(1)
burst = (rng.standard_normal(int(0.03 * sr)) * 0.15).astype(np.float32)
sig = np.zeros(int(sr * secs), np.float32)
starts = list(range(sr, len(sig) - sr, sr))
for s0 in starts: sig[s0:s0 + len(burst)] = burst
time.sleep(0.5)
sd.play(np.column_stack([sig, sig]), sr, device=OUT, blocking=True)
time.sleep(1.0)
for s in streams: s.stop(); s.close()
def assemble(key):
    t0 = chunks[key][0][0]
    return t0, np.concatenate([c for _, c in chunks[key]])
tr, ref = assemble('ref'); td, dst = assemble('dst')
offset = int(round((td - tr) * sr))  # dst sample i is at ref sample i + offset
env = lambda x: np.convolve(np.abs(x), np.ones(32) / 32, 'same')
out = []
for k in range(len(starts)):
    # locate burst k in ref by energy onset
    er = env(ref); thr = 0.05
    cand = np.flatnonzero(er > thr)
    if len(cand) == 0: break
onsets_ref = []
er = env(ref); i = 0
while i < len(er):
    if er[i] > 0.05:
        onsets_ref.append(i); i += int(0.5 * sr)
    else: i += 1
delays = []
for r0 in onsets_ref:
    d0 = r0 - offset
    seg = dst[max(0, d0 - 480): d0 + int(0.4 * sr)]
    c = np.abs(np.correlate(seg, burst, 'valid'))
    picked = []
    for p in np.argsort(c)[::-1]:
        if c[p] < 0.3 * c.max(): break
        if all(abs(p - q) > 480 for q in picked): picked.append(p)
        if len(picked) == 2: break
    # align to the burst start in ref using correlation too
    segr = ref[max(0, r0 - 480): r0 + 2400]
    pr = int(np.argmax(np.abs(np.correlate(segr, burst, 'valid')))) + max(0, r0 - 480)
    delays.append(sorted((max(0, d0 - 480) + p + offset - pr) / sr * 1000 for p in picked))
for d in delays: print('  ' + '  '.join(f'{x:7.1f}' for x in d))
an = np.array([d[-1] for d in delays if len(d) == 2])
if len(an): print(f'AudioNet path delay: median {np.median(an):.1f} ms, min {an.min():.1f}, max {an.max():.1f} ms ({len(an)} bursts)')
