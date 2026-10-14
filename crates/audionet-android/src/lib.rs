//! AudioNet's Android platform pieces, like `audionet-wasapi` on Windows
//! and `audionet-coreaudio` on macOS: a platform backend crate, allowed
//! `unsafe` (here only the hand-over of raw JVM pointers). Empty on other
//! platforms.
//!
//! Two parts of the engine need the app's Java context before they work:
//! cpal's AAudio backend (to list and open audio devices, through
//! `ndk-context`) and the TLS certificate verifier (the system's trust
//! store, through `rustls-platform-verifier`). The app calls
//! `AudioNetAndroid.attach(context)` once at startup, before anything else
//! in the engine runs; that is the JNI function below.
//!
//! Threads: called once on the app's main thread; the context is kept as a
//! global reference for the life of the process (the application context,
//! never an activity).

#[cfg(target_os = "android")]
mod attach;
