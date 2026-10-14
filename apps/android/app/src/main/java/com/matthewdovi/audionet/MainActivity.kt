package com.matthewdovi.audionet

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.LaunchedEffect
import com.matthewdovi.audionet.ui.AudioNetTheme
import com.matthewdovi.audionet.ui.Root

class MainActivity : ComponentActivity() {
    private val permissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
        val model = AppModel.get(this)
        if (granted[Manifest.permission.RECORD_AUDIO] == false) {
            model.note("Microphone access is off, so this phone's microphone cannot be sent. To allow it: Settings, Apps, AudioNet, Permissions, Microphone.")
        }
        model.refreshLocalAudio()
    }
    private var asked = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val model = AppModel.get(this)
        setContent {
            AudioNetTheme {
                Root(model)
                // Once signed in: the microphone (to send it, when chosen)
                // and notifications (the "AudioNet is online" one), asked
                // once per launch until answered.
                LaunchedEffect(model.signedIn) {
                    if (model.signedIn && !asked) {
                        asked = true
                        val wanted = buildList {
                            if (!model.microphoneAllowed) add(Manifest.permission.RECORD_AUDIO)
                            if (Build.VERSION.SDK_INT >= 33) add(Manifest.permission.POST_NOTIFICATIONS)
                        }
                        if (wanted.isNotEmpty()) permissions.launch(wanted.toTypedArray())
                    }
                }
            }
        }
    }
}
