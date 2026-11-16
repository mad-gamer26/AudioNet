package com.matthewdovi.audionet

import android.app.Application
import com.matthewdovi.audionet.engine.AudioNetAndroid
import com.matthewdovi.audionet.engine.PhoneVolumes

class AudioNetApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        // Before anything else in the engine: its audio devices and its
        // certificate checks need the application context.
        AudioNetAndroid.attach(this)
        // The phone's volumes, for the account's device control.
        PhoneVolumes.install(this)
        AppModel.get(this)
    }
}
