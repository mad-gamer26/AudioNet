# Streams a 997 Hz sine at amplitude 0.25 into a named WASAPI output, with
# constant memory, for long soak tests.
import sys, time, numpy as np, sounddevice as sd
name, secs = sys.argv[1], float(sys.argv[2])
w = [i for i,a in enumerate(sd.query_hostapis()) if 'WASAPI' in a['name']][0]
dev = next(i for i,d in enumerate(sd.query_devices()) if d['hostapi']==w and d['name'].startswith(name) and d['max_output_channels'] > 0)
sr, phase = 48000, 0.0
step = 2*np.pi*997/sr
def cb(out, frames, t, status):
    global phase
    ph = phase + step*np.arange(frames)
    out[:] = np.repeat((0.25*np.sin(ph)).astype(np.float32)[:,None], 2, axis=1)
    phase = float((ph[-1] + step) % (2*np.pi))
with sd.OutputStream(device=dev, samplerate=sr, channels=2, callback=cb):
    time.sleep(secs)
