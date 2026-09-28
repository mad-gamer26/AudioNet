# Verifies that a captured 32-bit float WAV contains a continuous sine (997 Hz
# by default) and reports frequency, level and sample-level discontinuities.
# Usage: python verify_wav.py FILE.wav [FREQUENCY_HZ]
import sys, wave, numpy as np
path = sys.argv[1]
with open(path, 'rb') as f: data = f.read()
# Minimal float WAV parse: find 'data' chunk.
i = data.find(b'data'); n = int.from_bytes(data[i+4:i+8], 'little')
fmt_i = data.find(b'fmt '); ch = int.from_bytes(data[fmt_i+10:fmt_i+12], 'little'); sr = int.from_bytes(data[fmt_i+12:fmt_i+16], 'little')
x = np.frombuffer(data[i+8:i+8+n], dtype='<f4').reshape(-1, ch)
print(f"frames={len(x)} channels={ch} rate={sr} seconds={len(x)/sr:.3f}")
left = x[:, 0].astype(np.float64)
nz = np.flatnonzero(np.abs(left) > 1e-4)
if len(nz) == 0: print("all silent"); sys.exit()
seg = left[nz[0]:nz[-1]+1]
print(f"signal from frame {nz[0]} to {nz[-1]}; channels identical: {np.allclose(x[:,0], x[:,1])}")
spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg))))
print(f"dominant frequency: {np.argmax(spec) * sr / len(seg):.2f} Hz")
print(f"peak={20*np.log10(np.max(np.abs(seg))):.2f} dBFS rms={20*np.log10(np.sqrt(np.mean(seg**2))):.2f} dBFS")
# Continuity: a pure sine satisfies x[n+1] + x[n-1] = 2cos(w) x[n]. Residual spikes mark gaps/glitches.
w = 2*np.pi*(float(sys.argv[2]) if len(sys.argv) > 2 else 997)/sr
res = seg[2:] + seg[:-2] - 2*np.cos(w)*seg[1:-1]
bad = np.flatnonzero(np.abs(res) > 1e-3)
print(f"max continuity residual={np.max(np.abs(res)):.2e}; discontinuities (residual > 1e-3): {len(bad)}" + (f" at frames {list((bad[:10]+nz[0]+1))}" if len(bad) else ""))
