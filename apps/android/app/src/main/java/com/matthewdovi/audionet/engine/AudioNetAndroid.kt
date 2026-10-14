package com.matthewdovi.audionet.engine

import android.content.Context

/**
 * The engine's Android setup (crates/audionet-android): hands the
 * application context to the audio library (to list and open audio
 * devices) and to the certificate verifier (the system's trust store for
 * the engine's secure connections). Call once, at startup, before anything
 * else in the engine.
 */
object AudioNetAndroid {
    init {
        // The engine library: the same one UniFFI's bindings load (by name)
        // through JNA.
        System.loadLibrary("audionet_ffi")
    }

    @JvmStatic
    external fun attach(context: Context)
}
