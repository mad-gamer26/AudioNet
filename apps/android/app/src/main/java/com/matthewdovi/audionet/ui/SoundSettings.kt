package com.matthewdovi.audionet.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import com.matthewdovi.audionet.AppModel
import kotlinx.coroutines.delay
import uniffi.audionet_ffi.SoundChange
import uniffi.audionet_ffi.SoundControl

/**
 * Another device's sound settings: its outputs and microphones (volume,
 * mute, default, default for communications, disable), a phone's own
 * volumes, and disabled devices to enable. Drawn from the live device
 * list, so changes made anywhere show at once.
 */
@Composable
fun SoundSettingsScreen(model: AppModel, modifier: Modifier) {
    val d = model.soundSettingsDevice()
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        if (d == null) {
            item { Text("This device is no longer visible.", Modifier.padding(top = 16.dp)) }
            return@LazyColumn
        }
        if (!d.allowsControl) {
            item { Text("${d.name} doesn't let other devices change its sound settings.", Modifier.padding(top = 16.dp)) }
            return@LazyColumn
        }
        val outputs = d.outputs.filter { it.control != null }
        val mics = d.sources.filter { it.isInput && it.control != null }
        if (outputs.isEmpty() && mics.isEmpty() && d.volumes.isEmpty() && d.disabled.isEmpty()) {
            item {
                Text(
                    if (d.platform == "ios") "An iPhone's sound settings can't be changed from other devices."
                    else "${d.name} doesn't report its sound settings. It may need a newer AudioNet.",
                    Modifier.padding(top = 16.dp),
                )
            }
        }
        if (outputs.isNotEmpty()) {
            item { Heading("Outputs") }
            items(outputs, key = { "o" + it.id }) { o ->
                EndpointSettings(o.name, o.id, o.isDefault, o.control!!, model::changeSound)
            }
        }
        if (mics.isNotEmpty()) {
            item { Heading("Microphones") }
            items(mics, key = { "m" + it.id }) { m ->
                EndpointSettings(m.name, m.id, m.isDefault, m.control!!, model::changeSound)
            }
        }
        if (d.volumes.isNotEmpty()) {
            item { Heading("Volumes") }
            items(d.volumes, key = { "v" + it.id }) { v ->
                Column {
                    VolumeSlider(v.name, v.volume) { model.changeSound(SoundChange.Volume(v.id, it)) }
                    val muted = v.muted
                    if (v.canMute && muted != null) {
                        SwitchRow("Mute ${v.name.lowercase()}", muted) { model.changeSound(SoundChange.Mute(v.id, it)) }
                    }
                }
            }
        }
        if (d.disabled.isNotEmpty()) {
            item { Heading("Disabled") }
            items(d.disabled, key = { "d" + it.id }) { x ->
                Column(Modifier.padding(vertical = 4.dp)) {
                    Text("${x.name} (${if (x.isInput) "microphone" else "output"}, disabled)")
                    OutlinedButton(
                        onClick = { model.changeSound(SoundChange.Enabled(x.id, true)) },
                        modifier = Modifier.semantics { contentDescription = "Enable ${x.name}" },
                    ) { Text("Enable") }
                }
            }
        }
    }
}

@Composable
private fun EndpointSettings(name: String, target: String, isDefault: Boolean, c: SoundControl, change: (SoundChange) -> Unit) {
    val roles = listOfNotNull(if (isDefault) "default" else null, if (c.isDefaultCommunications) "default for communications" else null)
    Column(Modifier.padding(vertical = 6.dp)) {
        Text(if (roles.isEmpty()) name else "$name (${roles.joinToString(", ")})", style = MaterialTheme.typography.titleSmall)
        val v = c.volume
        if (c.canSetVolume && v != null) {
            VolumeSlider(name, v) { change(SoundChange.Volume(target, it)) }
        } else if (v != null) {
            Text("Volume: ${Math.round(v * 100)}%")
        }
        val muted = c.muted
        if (c.canMute && muted != null) SwitchRow("Mute $name", muted) { change(SoundChange.Mute(target, it)) }
        if (c.canSetDefault && !isDefault) {
            OutlinedButton(onClick = { change(SoundChange.Default(target, false)) }) { Text("Make $name the Default") }
        }
        if (c.canSetDefaultCommunications && !c.isDefaultCommunications) {
            OutlinedButton(onClick = { change(SoundChange.Default(target, true)) }) { Text("Make $name the Default for Communications") }
        }
        if (c.canEnable) {
            OutlinedButton(onClick = { change(SoundChange.Enabled(target, false)) }) { Text("Disable $name") }
        }
    }
}

/**
 * A volume slider (0 to 100%) showing the device's value except while it
 * is moved; the value is sent once it rests for a tenth of a second (a drag
 * or TalkBack's steps), and when let go.
 */
@Composable
fun VolumeSlider(name: String, value: Float, send: (Float) -> Unit) {
    var moving by remember { mutableStateOf<Float?>(null) }
    val shown = moving ?: value
    LaunchedEffect(moving) {
        val m = moving ?: return@LaunchedEffect
        delay(100)
        send(m)
        // At rest (TalkBack's steps have no "let go"): show the device's
        // value again, which now includes the change.
        delay(700)
        if (moving == m) moving = null
    }
    Slider(
        value = shown,
        onValueChange = { moving = it },
        onValueChangeFinished = { moving?.let(send); moving = null },
        valueRange = 0f..1f,
        modifier = Modifier.semantics {
            contentDescription = "Volume of $name"
            stateDescription = "${Math.round(shown * 100)}%"
        },
    )
}
