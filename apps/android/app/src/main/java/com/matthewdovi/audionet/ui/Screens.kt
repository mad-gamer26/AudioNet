package com.matthewdovi.audionet.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.matthewdovi.audionet.AppModel
import com.matthewdovi.audionet.BuildConfig
import com.matthewdovi.audionet.StreamRow
import com.matthewdovi.audionet.accountName
import uniffi.audionet_ffi.Account
import uniffi.audionet_ffi.Device
import kotlin.math.roundToInt

/** Where the person is: the main screen, or one opened from it. */
enum class Screen { Main, Settings, StatusLog, AddAccount }

/** Sign-in until this phone belongs to an account, then the main screen. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Root(model: AppModel) {
    var screen by rememberSaveable { mutableStateOf(Screen.Main) }
    if (screen != Screen.Main) BackHandler { screen = Screen.Main }
    val title = when {
        !model.signedIn && !model.readingAccount -> "AudioNet"
        screen == Screen.Settings -> "Settings"
        screen == Screen.StatusLog -> "Status Log"
        screen == Screen.AddAccount -> "Add Account"
        else -> "AudioNet"
    }
    Scaffold(
        topBar = {
            Column {
                TopAppBar(
                    title = { Text(title, modifier = Modifier.semantics { heading() }) },
                    navigationIcon = {
                        if (screen != Screen.Main && model.signedIn) {
                            TextButton(onClick = { screen = Screen.Main }) { Text("Back") }
                        }
                    },
                )
                // The latest message, always on screen: TalkBack speaks it
                // when it changes (a polite live region).
                if (model.announcement.isNotEmpty()) {
                    Text(
                        model.announcement,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = 16.dp, vertical = 4.dp)
                            .semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
            }
        },
    ) { padding ->
        val modifier = Modifier.padding(padding).fillMaxSize()
        when {
            model.readingAccount -> Column(modifier.padding(16.dp)) { Text("Reading this phone's AudioNet sign-in…") }
            !model.signedIn -> SignInScreen(model, adding = false, modifier = modifier) {}
            screen == Screen.Settings -> SettingsScreen(model, modifier) { screen = it }
            screen == Screen.StatusLog -> StatusLogScreen(model, modifier)
            screen == Screen.AddAccount -> SignInScreen(model, adding = true, modifier = modifier) { screen = Screen.Settings }
            else -> MainScreen(model, modifier) { screen = it }
        }
    }
}

/** A section heading. */
@Composable
fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleMedium,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 20.dp, bottom = 6.dp)
            .semantics { heading() },
    )
}

/** A switch with its label, one control for TalkBack ("Share This Phone's Audio, switch, on"). */
@Composable
fun SwitchRow(label: String, checked: Boolean, enabled: Boolean = true, onChange: (Boolean) -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .toggleable(value = checked, enabled = enabled, role = Role.Switch, onValueChange = onChange),
    ) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = null, enabled = enabled)
    }
}

/** One choice among several (radio buttons in a group with its title). */
@Composable
fun <T> ChoiceGroup(title: String, options: List<T>, selected: (T) -> Boolean, label: (T) -> String, onSelect: (T) -> Unit) {
    Text(title, style = MaterialTheme.typography.labelLarge, modifier = Modifier.padding(top = 8.dp).semantics { heading() })
    Column(Modifier.selectableGroup()) {
        for (o in options) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .selectable(selected = selected(o), role = Role.RadioButton, onClick = { onSelect(o) }),
            ) {
                RadioButton(selected = selected(o), onClick = null)
                Spacer(Modifier.width(8.dp))
                Text(label(o))
            }
        }
    }
}

// MARK: Sign in

@Composable
fun SignInScreen(model: AppModel, adding: Boolean, modifier: Modifier, done: () -> Unit) {
    val context = LocalContext.current
    var server by rememberSaveable { mutableStateOf(model.defaultServer) }
    var username by rememberSaveable { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var deviceName by rememberSaveable { mutableStateOf(model.phoneName) }
    fun submit() {
        if (listOf(server, username, password, deviceName).any { it.isBlank() }) {
            model.reportSignInProblem("Enter the server address, your account name and password, and a name for this phone.")
            return
        }
        val pw = password
        password = "" // not kept
        model.signIn(server.trim(), username.trim(), pw, deviceName.trim()) { ok -> if (ok && adding) done() }
    }
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        item { Heading(if (adding) "Add an account" else "Sign in") }
        item {
            OutlinedTextField(server, { server = it }, label = { Text("Server address") },
                placeholder = { Text("https://audionet.example.com") },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth())
        }
        item {
            OutlinedTextField(username, { username = it }, label = { Text("Account name") }, singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Next), modifier = Modifier.fillMaxWidth())
        }
        item {
            OutlinedTextField(password, { password = it }, label = { Text("Password") }, singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth())
        }
        item {
            OutlinedTextField(deviceName, { deviceName = it }, label = { Text("Name for this phone") }, singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), modifier = Modifier.fillMaxWidth())
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp), modifier = Modifier.padding(top = 12.dp)) {
                Button(onClick = ::submit, enabled = !model.signingIn) { Text(if (model.signingIn) "Signing In…" else "Sign In") }
                // Passwords are reset in the server's web client.
                OutlinedButton(onClick = {
                    val base = server.trim().trimEnd('/')
                    if (!base.startsWith("https://") && !base.startsWith("http://")) {
                        model.reportSignInProblem("Enter the server address first, starting with https://.")
                    } else {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("$base/?forgot")))
                    }
                }) { Text("Forgot Password?") }
            }
        }
        model.signInProblem?.let { item { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 8.dp)) } }
        item {
            Text(
                if (adding) "Use the other AudioNet account. This phone becomes a device there too; the password is used once and is not stored."
                else "Use your AudioNet account. The password is used once to add this phone and is not stored.",
                modifier = Modifier.padding(top = 12.dp),
            )
        }
    }
}

// MARK: Main screen

@Composable
fun MainScreen(model: AppModel, modifier: Modifier, open: (Screen) -> Unit) {
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        item { Heading("This phone") }
        item {
            Text("Status: ${statusText(model)}", modifier = Modifier.padding(vertical = 8.dp))
        }
        for (a in model.accounts.toList()) {
            item(key = "share-${a.nodeId}") {
                val label = if (model.accounts.size > 1) "Share This Phone's Audio in ${a.accountName}" else "Share This Phone's Audio"
                SwitchRow(label, model.isSharing(a.nodeId), enabled = !model.signingOut.contains(a.nodeId)) { model.setSharing(a.nodeId, it) }
            }
        }
        if (model.microphones.isNotEmpty()) {
            item {
                ChoiceGroup("Microphone", listOf<String?>(null) + model.microphones.map { it.id },
                    selected = { it == model.chosenMicrophone },
                    label = { id -> model.microphones.firstOrNull { it.id == id }?.name ?: "Phone default" },
                    onSelect = { model.chooseMicrophone(it) })
            }
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp), modifier = Modifier.padding(top = 8.dp)) {
                OutlinedButton(onClick = { open(Screen.StatusLog) }) { Text("Status Log") }
                OutlinedButton(onClick = { open(Screen.Settings) }) { Text("Settings") }
            }
        }
        for (a in model.accounts.toList()) {
            item(key = "head-${a.nodeId}") { Heading(if (model.accounts.size > 1) "Devices in ${a.accountName}" else "Your devices") }
            val list = model.devicesByAccount[a.nodeId].orEmpty()
            when {
                !model.connectedAccounts.contains(a.nodeId) -> item { Text("Connecting…") }
                !model.devicesLoaded.contains(a.nodeId) -> item { Text("Looking for your devices…") }
                list.isEmpty() -> item { Text("No other devices yet.") }
                else -> deviceItems(model, list)
            }
        }
        item { Heading("Streams started here") }
        if (model.streams.isEmpty()) {
            item { Text("No streams are running.") }
        } else {
            for (row in model.streams.toList()) {
                item(key = "stream-${row.id}") { StreamControls(model, row) }
            }
        }
        item { Spacer(Modifier.heightIn(min = 32.dp)) }
    }
}

private fun LazyListScope.deviceItems(model: AppModel, list: List<Device>) {
    for (d in list) {
        item(key = "dev-${d.nodeId}") {
            val open = model.expanded.contains(d.nodeId)
            // One row per device, opening its controls: "Studio PC: online,
            // collapsed, double-tap to expand".
            Text(
                "${d.name}: ${deviceState(d)}",
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .clickable(onClickLabel = if (open) "collapse" else "expand", role = Role.Button) { model.toggleExpanded(d.nodeId) }
                    .semantics { stateDescription = if (open) "expanded" else "collapsed" }
                    .padding(vertical = 12.dp),
            )
            if (open) DeviceControls(model, d)
            HorizontalDivider()
        }
    }
}

/** Listen to and send to one device (under its row). */
@Composable
fun DeviceControls(model: AppModel, d: Device) {
    val ch = model.choices[d.nodeId]
    Column(Modifier.padding(start = 16.dp, bottom = 12.dp)) {
        if (d.sources.isNotEmpty()) {
            ChoiceGroup("Sound to listen to", d.sources, selected = { it.id == ch?.listenSource },
                label = { s -> "${s.name} (${if (s.isInput) "input" else "what it plays"}${if (s.isDefault) ", default" else ""})" },
                onSelect = { s -> model.choose(d.nodeId) { it.copy(listenSource = s.id) } })
        }
        if (d.online && !d.sharing) Text("${d.name} is not sharing its audio: it cannot be listened to, but you can send to it.")
        Button(
            onClick = { model.listen(d) },
            enabled = model.online && d.online && d.sharing,
            modifier = Modifier.padding(top = 8.dp).semantics { contentDescription = "Listen to ${d.name}" },
        ) { Text("Listen") }
        if (d.outputs.isNotEmpty()) {
            ChoiceGroup("Play my microphone on", d.outputs, selected = { it.id == ch?.sendOutput },
                label = { o -> if (o.isDefault) "${o.name} (default)" else o.name },
                onSelect = { o -> model.choose(d.nodeId) { it.copy(sendOutput = o.id) } })
        }
        val sharingHere = model.accountOf(d)?.let { model.isSharing(it) } ?: false
        if (!sharingHere) Text("Turn on Share This Phone's Audio to send its microphone.")
        Button(
            onClick = { model.send(d) },
            enabled = model.online && d.online && sharingHere,
            modifier = Modifier.padding(top = 8.dp).semantics { contentDescription = "Send my microphone to ${d.name}" },
        ) { Text("Send My Microphone") }
    }
}

/** A stream: what and its state, measurements (when on), mute, volume and Stop. */
@Composable
fun StreamControls(model: AppModel, row: StreamRow) {
    Column(Modifier.padding(vertical = 8.dp)) {
        Text("${row.title}: ${row.state}")
        if (model.showMeasurements) model.diagnostics[row.id]?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        SwitchRow("Mute ${row.title}", row.muted) { model.setVolume(row.id, muted = it) }
        val percent = (row.volume * 100).roundToInt()
        Text("Volume", style = MaterialTheme.typography.labelLarge)
        Slider(
            value = row.volume,
            onValueChange = { model.setVolume(row.id, volume = (it * 20).roundToInt() / 20f) },
            steps = 19,
            modifier = Modifier.semantics {
                contentDescription = "Volume for ${row.title}"
                stateDescription = if (row.muted) "$percent percent, muted" else "$percent percent"
            },
        )
        OutlinedButton(onClick = { model.stopStream(row) }, modifier = Modifier.semantics { contentDescription = "Stop ${row.title}" }) { Text("Stop") }
        HorizontalDivider(Modifier.padding(top = 8.dp))
    }
}

fun deviceState(d: Device): String = if (!d.online) "offline" else if (d.sharing) "online" else "online, not sharing"

private fun statusText(model: AppModel): String {
    if (model.accounts.isEmpty()) return "Not signed in"
    if (!model.connected) return "Connecting…"
    val sharing = model.sharingAccounts.size
    if (model.accounts.size == 1) return if (sharing == 1) "Online, sharing its audio" else "Online, not sharing its audio"
    return "Online in ${model.accounts.size} accounts, sharing in $sharing"
}

// MARK: Status log and settings

@Composable
fun StatusLogScreen(model: AppModel, modifier: Modifier) {
    val context = LocalContext.current
    Column(modifier.padding(horizontal = 16.dp)) {
        OutlinedButton(onClick = {
            val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(ClipData.newPlainText("AudioNet status log", model.log.joinToString("\n")))
            model.announce("Status copied.")
        }, modifier = Modifier.padding(vertical = 8.dp).semantics { contentDescription = "Copy Status Log" }) { Text("Copy") }
        // One row per line: TalkBack moves through them one by one.
        LazyColumn {
            items(model.log.takeLast(200)) { line -> Text(line, modifier = Modifier.padding(vertical = 6.dp)) }
        }
    }
}

@Composable
fun SettingsScreen(model: AppModel, modifier: Modifier, open: (Screen) -> Unit) {
    var signingOut by remember { mutableStateOf<Account?>(null) }
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        item { Heading("Accounts") }
        for (a in model.accounts.toList()) {
            item(key = a.nodeId) {
                Text("Signed in as \"${a.deviceName}\" to account ${a.username} on ${a.serverUrl}.", modifier = Modifier.padding(top = 8.dp))
                OutlinedButton(
                    onClick = { signingOut = a },
                    enabled = !model.signingOut.contains(a.nodeId),
                    modifier = Modifier.semantics { contentDescription = "Sign Out of ${a.accountName}" },
                ) { Text(if (model.signingOut.contains(a.nodeId)) "Signing Out…" else "Sign Out") }
            }
        }
        item { OutlinedButton(onClick = { open(Screen.AddAccount) }, modifier = Modifier.padding(top = 8.dp)) { Text("Add Account") } }
        item { Heading("Troubleshooting") }
        item { SwitchRow("Show Measurements", model.showMeasurements) { model.changeShowMeasurements(it) } }
        item { Text("Numbers about streams and the network, under each stream and in the status log, for troubleshooting.") }
        item { Heading("About") }
        item { Text("Version: ${BuildConfig.VERSION_NAME}") }
        item { Text("Engine version: ${uniffi.audionet_ffi.engineVersion()}") }
    }
    signingOut?.let { a ->
        AlertDialog(
            onDismissRequest = { signingOut = null },
            title = { Text("Sign Out") },
            text = { Text("Sign out of ${a.accountName}? This phone is removed from that account and stops sharing there. To use it there again, sign in again.") },
            confirmButton = { TextButton(onClick = { model.signOut(a); signingOut = null }) { Text("Sign Out") } },
            dismissButton = { TextButton(onClick = { signingOut = null }) { Text("Cancel") } },
        )
    }
}
