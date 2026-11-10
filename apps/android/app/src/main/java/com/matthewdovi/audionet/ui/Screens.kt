package com.matthewdovi.audionet.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Settings
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
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
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
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.customActions
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

/** The signed-in tabs (the bottom navigation bar), in order. */
enum class Tab(val title: String) { Devices("Devices"), Streams("Streams"), Accounts("Accounts"), Settings("Settings") }

/** A screen opened from a tab (Back returns to the tab). */
enum class Sub { None, StatusLog, AddAccount }

/**
 * Sign-in until this phone belongs to an account; then a standard bottom
 * navigation bar with Devices, Streams, Accounts and Settings (TalkBack:
 * "Devices, tab, 1 of 4, selected").
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Root(model: AppModel) {
    var tab by rememberSaveable { mutableStateOf(Tab.Devices) }
    var sub by rememberSaveable { mutableStateOf(Sub.None) }
    if (sub != Sub.None) BackHandler { sub = Sub.None }
    val tabs = model.signedIn && !model.readingAccount
    val title = when {
        !tabs -> "AudioNet"
        sub == Sub.StatusLog -> "Status Log"
        sub == Sub.AddAccount -> "Add Account"
        else -> tab.title
    }
    Scaffold(
        topBar = {
            Column {
                TopAppBar(
                    title = { Text(title, modifier = Modifier.semantics { heading() }) },
                    navigationIcon = {
                        if (sub != Sub.None && tabs) {
                            TextButton(onClick = { sub = Sub.None }) { Text("Back") }
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
        bottomBar = {
            if (tabs) {
                NavigationBar {
                    for (t in Tab.entries) {
                        val label = if (t == Tab.Streams && model.streams.isNotEmpty()) "Streams (${model.streams.size})" else t.title
                        NavigationBarItem(
                            selected = tab == t && sub == Sub.None,
                            onClick = { tab = t; sub = Sub.None },
                            // Decorative: the label names the tab.
                            icon = { Icon(t.icon(), contentDescription = null) },
                            label = { Text(label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        val modifier = Modifier.padding(padding).fillMaxSize()
        when {
            model.readingAccount -> Column(modifier.padding(16.dp)) { Text("Reading this phone's AudioNet sign-in…") }
            !model.signedIn -> SignInScreen(model, adding = false, modifier = modifier) {}
            sub == Sub.StatusLog -> StatusLogScreen(model, modifier)
            sub == Sub.AddAccount -> SignInScreen(model, adding = true, modifier = modifier) { sub = Sub.None }
            tab == Tab.Devices -> DevicesScreen(model, modifier)
            tab == Tab.Streams -> StreamsScreen(model, modifier)
            tab == Tab.Accounts -> AccountsScreen(model, modifier) { sub = Sub.AddAccount }
            else -> SettingsScreen(model, modifier) { sub = Sub.StatusLog }
        }
    }
}

private fun Tab.icon(): ImageVector = when (this) {
    Tab.Devices -> Icons.Filled.Home
    Tab.Streams -> Icons.Filled.PlayArrow
    Tab.Accounts -> Icons.Filled.Person
    Tab.Settings -> Icons.Filled.Settings
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

/** A switch with its label, one control for TalkBack ("Visible to My Other Devices, switch, on"). */
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

// MARK: Devices

@Composable
fun DevicesScreen(model: AppModel, modifier: Modifier) {
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        item { Heading("This phone") }
        item {
            Text("Status: ${statusText(model)}", modifier = Modifier.padding(vertical = 8.dp))
        }
        if (model.microphones.isNotEmpty()) {
            item {
                ChoiceGroup("Microphone", listOf<String?>(null) + model.microphones.map { it.id },
                    selected = { it == model.chosenMicrophone },
                    label = { id -> model.microphones.firstOrNull { it.id == id }?.name ?: "Phone default" },
                    onSelect = { model.chooseMicrophone(it) })
            }
        }
        for (a in model.accounts.toList()) {
            item(key = "head-${a.nodeId}") { Heading(if (model.accounts.size > 1) "Devices in ${a.accountName}" else "Your devices") }
            val list = model.devicesByAccount[a.nodeId].orEmpty()
            when {
                !model.connectedAccounts.contains(a.nodeId) -> item { Text("Connecting…") }
                !model.devicesLoaded.contains(a.nodeId) -> item { Text("Looking for your devices…") }
                list.isEmpty() -> item { Text("No other devices are visible. Devices appear here while AudioNet runs on them with visibility on.") }
                else -> deviceItems(model, list)
            }
        }
        item { Spacer(Modifier.heightIn(min = 32.dp)) }
    }
}

// MARK: Streams

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StreamsScreen(model: AppModel, modifier: Modifier) {
    // The stream whose sheet is open (it closes by itself when the stream ends).
    var openId by rememberSaveable { mutableStateOf<String?>(null) }
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        if (model.streams.isEmpty()) {
            item { Text("No streams are running. Start one from Devices: open a device, then Listen or Send.", modifier = Modifier.padding(vertical = 12.dp)) }
        } else {
            for (row in model.streams.toList()) {
                item(key = "stream-${row.id}") { StreamRowItem(model, row) { openId = row.id } }
            }
        }
        item { Spacer(Modifier.heightIn(min = 32.dp)) }
    }
    val open = model.streams.firstOrNull { it.id == openId }
    if (open != null) {
        // Fully open at once: a half-open sheet can leave Stop and Done
        // below the screen's edge.
        ModalBottomSheet(onDismissRequest = { openId = null }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
            StreamSheet(model, open) { openId = null }
        }
    }
}

/**
 * A stream's row: what it is and its state, its volume as its state
 * description, the measurements when shown, and TalkBack actions in this
 * order: Stop, Decrease Volume, Increase Volume, Mute or Unmute. Increase is
 * offered only below 100 percent, Decrease only above 0. A tap opens its sheet.
 */
@Composable
fun StreamRowItem(model: AppModel, row: StreamRow, open: () -> Unit) {
    val actions = buildList {
        add(CustomAccessibilityAction("Stop") { model.stopStream(row); true })
        if (model.volumeStep(row.id, up = false) != null) {
            add(CustomAccessibilityAction("Decrease Volume") { model.stepVolume(row.id, up = false); true })
        }
        if (model.volumeStep(row.id, up = true) != null) {
            add(CustomAccessibilityAction("Increase Volume") { model.stepVolume(row.id, up = true); true })
        }
        add(CustomAccessibilityAction(if (row.muted) "Unmute" else "Mute") { model.toggleMute(row.id); true })
    }
    Column(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clickable(onClickLabel = "open its controls", role = Role.Button, onClick = open)
            .semantics(mergeDescendants = true) {
                stateDescription = "Volume ${model.volumeText(row)}"
                customActions = actions
            }
            .padding(vertical = 12.dp),
    ) {
        Text("${row.title}: ${row.state}")
        // Measurements only when "Show Measurements" is on (Settings); never announced.
        if (model.showMeasurements) model.diagnostics[row.id]?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
    HorizontalDivider()
}

/** One stream's sheet: its title as the heading, its state, Mute, Volume, Stop and Done. */
@Composable
fun StreamSheet(model: AppModel, row: StreamRow, close: () -> Unit) {
    Column(Modifier.padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
        Text(
            row.title,
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(bottom = 8.dp).semantics { heading() },
        )
        Text(row.state.replaceFirstChar { it.uppercase() }, modifier = Modifier.padding(bottom = 8.dp))
        SwitchRow("Mute", row.muted) { model.setVolume(row.id, muted = it) }
        Text("Volume", style = MaterialTheme.typography.labelLarge)
        Slider(
            value = row.volume,
            onValueChange = { model.setVolume(row.id, volume = (it * 20).roundToInt() / 20f) },
            steps = 19,
            modifier = Modifier.semantics {
                contentDescription = "Volume"
                stateDescription = model.volumeText(row)
            },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp), modifier = Modifier.padding(top = 8.dp)) {
            OutlinedButton(onClick = { model.stopStream(row); close() }) { Text("Stop") }
            Button(onClick = close) { Text("Done") }
        }
    }
}

// MARK: Accounts

@Composable
fun AccountsScreen(model: AppModel, modifier: Modifier, addAccount: () -> Unit) {
    val context = LocalContext.current
    var signingOut by remember { mutableStateOf<Account?>(null) }
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        for (a in model.accounts.toList()) {
            item(key = a.nodeId) {
                Heading(if (model.accounts.size > 1) a.accountName else "Account")
                Text("Signed in as \"${a.deviceName}\" to account ${a.username} on ${a.serverUrl}.", modifier = Modifier.padding(bottom = 8.dp))
                val label = if (model.accounts.size > 1) "Visible to My Other Devices in ${a.accountName}" else "Visible to My Other Devices"
                SwitchRow(label, model.isVisible(a.nodeId), enabled = !model.signingOut.contains(a.nodeId)) { model.setVisible(a.nodeId, it) }
                Text(
                    if (model.isVisible(a.nodeId)) "Your other devices see this phone, and can listen to it and send to it."
                    else "Your other devices cannot see this phone or reach it. It can still listen to them and send to them.",
                    modifier = Modifier.padding(bottom = 8.dp),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlinedButton(
                        onClick = { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(a.serverUrl.trimEnd('/') + "/?manage"))) },
                        modifier = Modifier.semantics { contentDescription = if (model.accounts.size > 1) "Manage Devices in ${a.accountName}" else "Manage Devices" },
                    ) { Text("Manage Devices") }
                    OutlinedButton(
                        onClick = { signingOut = a },
                        enabled = !model.signingOut.contains(a.nodeId),
                        modifier = Modifier.semantics { contentDescription = "Sign Out of ${a.accountName}" },
                    ) { Text(if (model.signingOut.contains(a.nodeId)) "Signing Out…" else "Sign Out") }
                }
            }
        }
        item {
            Column(Modifier.padding(top = 20.dp)) {
                OutlinedButton(onClick = addAccount) { Text("Add Account") }
                Text("Renaming and removing your devices is in Manage Devices, in the web client.", modifier = Modifier.padding(top = 8.dp))
            }
        }
        item { Spacer(Modifier.heightIn(min = 32.dp)) }
    }
    signingOut?.let { a ->
        AlertDialog(
            onDismissRequest = { signingOut = null },
            title = { Text("Sign Out") },
            text = { Text("Sign out of ${a.accountName}? This phone is removed from that account. To use it there again, sign in again.") },
            confirmButton = { TextButton(onClick = { model.signOut(a); signingOut = null }) { Text("Sign Out") } },
            dismissButton = { TextButton(onClick = { signingOut = null }) { Text("Cancel") } },
        )
    }
}

private fun LazyListScope.deviceItems(model: AppModel, list: List<Device>) {
    for (d in list) {
        item(key = "dev-${d.nodeId}") {
            val open = model.expanded.contains(d.nodeId)
            // One row per device, opening its controls: "Studio PC,
            // Windows, collapsed, double-tap to expand".
            Text(
                deviceLabel(d),
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
        Button(
            onClick = { model.listen(d) },
            enabled = model.online,
            modifier = Modifier.padding(top = 8.dp).semantics { contentDescription = "Listen to ${d.name}" },
        ) { Text("Listen") }
        if (d.outputs.isNotEmpty()) {
            ChoiceGroup("Play my microphone on", d.outputs, selected = { it.id == ch?.sendOutput },
                label = { o -> if (o.isDefault) "${o.name} (default)" else o.name },
                onSelect = { o -> model.choose(d.nodeId) { it.copy(sendOutput = o.id) } })
        }
        Button(
            onClick = { model.send(d) },
            enabled = model.online,
            modifier = Modifier.padding(top = 8.dp).semantics { contentDescription = "Send my microphone to ${d.name}" },
        ) { Text("Send My Microphone") }
    }
}

/** A device's platform in words, or empty when unknown. */
fun platformName(p: String): String = when (p) {
    "windows" -> "Windows"
    "macos" -> "Mac"
    "ios" -> "iPhone"
    "android" -> "Android"
    "linux" -> "Linux"
    "web" -> "Web browser"
    "" -> ""
    else -> p.replaceFirstChar { it.uppercase() }
}

/** A device in a list: "Studio PC, Windows". Every listed device is visible. */
fun deviceLabel(d: Device): String = platformName(d.platform).let { if (it.isEmpty()) d.name else "${d.name}, $it" }

private fun statusText(model: AppModel): String {
    if (model.accounts.isEmpty()) return "Not signed in"
    if (!model.connected) return "Connecting…"
    val visible = model.visibleAccounts.size
    if (model.accounts.size == 1) return if (visible == 1) "Visible to your other devices" else "Hidden from your other devices"
    return "Visible in $visible of ${model.accounts.size} accounts"
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
fun SettingsScreen(model: AppModel, modifier: Modifier, openLog: () -> Unit) {
    LazyColumn(modifier.padding(horizontal = 16.dp)) {
        item { Heading("Troubleshooting") }
        item { SwitchRow("Show Measurements", model.showMeasurements) { model.changeShowMeasurements(it) } }
        item { Text("Numbers about streams and the network, under each stream and in the status log, for troubleshooting.") }
        item { OutlinedButton(onClick = openLog, modifier = Modifier.padding(top = 8.dp)) { Text("Status Log") } }
        item { Heading("About") }
        item { Text("Version: ${BuildConfig.VERSION_NAME}") }
        item { Text("Engine version: ${uniffi.audionet_ffi.engineVersion()}") }
    }
}
