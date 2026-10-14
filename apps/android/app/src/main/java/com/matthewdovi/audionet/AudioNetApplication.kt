package com.matthewdovi.audionet

import android.app.Application
import com.matthewdovi.audionet.engine.AudioNetAndroid

class AudioNetApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        // Before anything else in the engine: its audio devices and its
        // certificate checks need the application context.
        AudioNetAndroid.attach(this)
        AppModel.get(this)
    }
}
