package com.matthewdovi.audionet

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat
import uniffi.audionet_ffi.Account
import uniffi.audionet_ffi.AudioNetException
import uniffi.audionet_ffi.AudioUseListener
import uniffi.audionet_ffi.Client
import uniffi.audionet_ffi.Device
import uniffi.audionet_ffi.Event
import uniffi.audionet_ffi.EventListener
import uniffi.audionet_ffi.LocalAudio
import uniffi.audionet_ffi.StreamState
import java.util.concurrent.Executors

/** A stream this phone started. */
data class StreamRow(
    val id: String,
    /** The account (this phone's device id in it) the stream runs in. */
    val account: String,
    val title: String,
    val nodeId: String,
    val deviceName: String,
    val listening: Boolean,
    val state: String,
    /** Volume slider position (0 to 1) and mute, on this phone. */
    val volume: Float = 1f,
    val muted: Boolean = false,
)

/** What to listen to and send, per device (node id). */
data class DeviceChoices(
    val listenSource: String? = null,
    val listenOutput: String? = null,
    val sendSource: String? = null,
    val sendOutput: String? = null,
)

/**
 * Everything the screens show, and every action they take: the Android
 * counterpart of the iPhone app's model.
 *
 * The phone can be signed in to several AudioNet accounts; it is a separate
 * device in each, with its own connection (engine client). While AudioNet
 * runs and is signed in, the phone is online in all of them (OnlineService
 * keeps it so with the screen off).
 *
 * All state changes happen on the main thread: engine events arrive on
 * engine threads and are posted here; blocking engine calls (sign-in,
 * sign-out, device lists) run on a background thread.
 */
class AppModel(private val context: Context) {
    private val store = AccountStore(context)
    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()

    val accounts = mutableStateListOf<Account>()
    var readingAccount by mutableStateOf(false); private set
    var online by mutableStateOf(false); private set
    /** Accounts in which this phone shares its audio (by its device id there). */
    val sharingAccounts = mutableStateListOf<String>()
    val signingOut = mutableStateListOf<String>()
    val connectedAccounts = mutableStateListOf<String>()
    var signingIn by mutableStateOf(false); private set
    var signInProblem by mutableStateOf<String?>(null)
    /** Each account's devices (by this phone's device id there). */
    val devicesByAccount = mutableStateMapOf<String, List<Device>>()
    val devicesLoaded = mutableStateListOf<String>()
    var local by mutableStateOf(LocalAudio(emptyList(), emptyList())); private set
    val streams = mutableStateListOf<StreamRow>()
    val log = mutableStateListOf<String>()
    /** The latest message worth speaking; the screen shows it in a live region. */
    var announcement by mutableStateOf("")
    val diagnostics = mutableStateMapOf<String, String>()
    val choices = mutableStateMapOf<String, DeviceChoices>()
    val expanded = mutableStateListOf<String>()
    /** Streams on this phone, including those other devices started. */
    var engineStreams by mutableStateOf(0); private set

    var showMeasurements by mutableStateOf(store.settings.getBoolean("showMeasurements", false))
        private set

    /** The microphone chosen to send (a local source id), or null for the default. */
    var microphone by mutableStateOf(store.settings.getString("microphone", null))
        private set

    private val clients = mutableMapOf<String, Client>()
    private val generations = mutableMapOf<String, Int>()

    val signedIn: Boolean get() = accounts.isNotEmpty()
    val connected: Boolean get() = online && accounts.isNotEmpty() && accounts.all { connectedAccounts.contains(it.nodeId) }
    val devices: List<Device> get() = accounts.flatMap { devicesByAccount[it.nodeId].orEmpty() }

    val accountList: String
        get() = accounts.joinToString(" and ") { it.accountName }

    val defaultServer: String
        get() = accounts.firstOrNull()?.serverUrl ?: BuildConfig.DEFAULT_SERVER

    /** This phone's name as the person set it (Settings, About phone), else its model. */
    val phoneName: String
        get() = Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() } ?: Build.MODEL

    init {
        uniffi.audionet_ffi.setShowMeasurements(showMeasurements)
        if (!store.hasStoredAccount) {
            note("To add this phone, sign in with your AudioNet account name and password.")
        } else {
            readingAccount = true
            background.execute {
                val (read, problem) = store.loadAll()
                main.post { accountsRead(read, problem) }
            }
        }
    }

    private fun accountsRead(read: List<Account>, problem: String?) {
        readingAccount = false
        accounts.clear()
        accounts.addAll(read)
        problem?.let { announce(it) }
        if (read.isEmpty()) {
            if (problem == null) note("To add this phone, sign in with your AudioNet account name and password.")
            return
        }
        note("Welcome back. This phone is signed in to $accountList.")
        connectAll()
    }

    // MARK: Log and announcements

    fun note(text: String) {
        log.add(text)
        if (log.size > 300) log.removeRange(0, log.size - 300)
    }

    /** Writes to the log and has TalkBack say it (through the live region). */
    fun announce(text: String) {
        note(text)
        announcement = text
    }

    fun reportSignInProblem(text: String) {
        signInProblem = text
        announce(text)
    }

    fun changeShowMeasurements(on: Boolean) {
        showMeasurements = on
        store.settings.edit().putBoolean("showMeasurements", on).apply()
        uniffi.audionet_ffi.setShowMeasurements(on)
    }

    // MARK: Accounts

    /** Adds this phone to an account; `done` gets true once signed in. */
    fun signIn(server: String, username: String, password: String, deviceName: String, done: (Boolean) -> Unit = {}) {
        if (signingIn) return
        val wanted = Account(server, "", "", deviceName, username)
        accounts.firstOrNull { it.sameAccount(wanted) }?.let {
            reportSignInProblem("This phone is already signed in to ${it.accountName}.")
            return
        }
        signingIn = true
        signInProblem = null
        announce("Signing in.")
        background.execute {
            val result = runCatching { uniffi.audionet_ffi.signIn(server, username, password, deviceName) }
            main.post {
                signingIn = false
                result.onSuccess { account ->
                    accounts.add(account)
                    store.saveAll(accounts)?.let { announce(it) }
                    announce("Signed in as \"${account.deviceName}\" to ${account.accountName}. This phone is online there, not sharing its audio: turn on sharing to share it.")
                    connectAll()
                    connect(account)
                    done(true)
                }.onFailure { e ->
                    val text = "Signing in failed: ${describe(e)}" +
                        if (describe(e).contains("password is incorrect")) " If you forgot the password, choose Forgot Password." else ""
                    reportSignInProblem(text)
                    done(false)
                }
            }
        }
    }

    /**
     * Signs this phone out of one account: its connection stops, then the
     * server removes this phone's device from the account. If the server
     * cannot be reached, it says so and stays signed in there.
     */
    fun signOut(account: Account) {
        val id = account.nodeId
        if (signingOut.contains(id)) return
        signingOut.add(id)
        disconnect(id)
        announce("Signing out of ${account.accountName}.")
        background.execute {
            val result = runCatching { uniffi.audionet_ffi.removeDevice(account) }
            main.post {
                signingOut.remove(id)
                result.onSuccess {
                    accounts.removeAll { it.nodeId == id }
                    sharingAccounts.remove(id)
                    store.settings.edit().remove("sharing-$id").apply()
                    store.saveAll(accounts)?.let { announce(it) }
                    if (accounts.isEmpty()) disconnectAll()
                    announce("Signed out of ${account.accountName}: this phone was removed from that account.")
                }.onFailure { e ->
                    connect(account)
                    announce("Could not sign out of ${account.accountName}: ${describe(e)} This phone is still signed in there.")
                }
            }
        }
    }

    // MARK: Connections

    /** Connects every account: while AudioNet runs, this phone is online in all of them. */
    fun connectAll() {
        if (accounts.isEmpty() || online) return
        uniffi.audionet_ffi.setAudioUseListener(audioUse)
        online = true
        OnlineService.update(context, this)
        accounts.toList().forEach { connect(it) }
        refreshLocalAudio()
    }

    private fun connect(account: Account) {
        val id = account.nodeId
        if (clients.containsKey(id)) return
        val generation = (generations[id] ?: 0) + 1
        generations[id] = generation
        val listener = object : EventListener {
            override fun onEvent(event: Event) {
                main.post { if (generations[id] == generation) handle(event, id) }
            }
        }
        val client = Client(account, listener)
        val share = store.settings.getBoolean("sharing-$id", false)
        client.setSharing(share)
        if (share) { if (!sharingAccounts.contains(id)) sharingAccounts.add(id) } else sharingAccounts.remove(id)
        clients[id] = client
        client.start()
    }

    private fun disconnect(id: String) {
        generations[id] = (generations[id] ?: 0) + 1
        connectedAccounts.remove(id)
        devicesByAccount.remove(id)
        devicesLoaded.remove(id)
        streams.filter { it.account == id }.forEach { diagnostics.remove(it.id) }
        streams.removeAll { it.account == id }
        clients.remove(id)?.let { c -> background.execute { c.stop(); c.close() } }
    }

    private fun disconnectAll() {
        if (!online) return
        clients.keys.toList().forEach { disconnect(it) }
        clearOnlineState()
    }

    private fun clearOnlineState() {
        online = false
        sharingAccounts.clear()
        connectedAccounts.clear()
        streams.clear()
        devicesByAccount.clear()
        devicesLoaded.clear()
        expanded.clear()
        diagnostics.clear()
        OnlineService.update(context, this)
    }

    fun refreshLocalAudio() {
        background.execute {
            val audio = runCatching { uniffi.audionet_ffi.localAudio() }.getOrElse { LocalAudio(emptyList(), emptyList()) }
            main.post {
                local = audio
                devices.forEach { fillChoices(it) }
            }
        }
    }

    val microphoneAllowed: Boolean
        get() = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

    /** The phone's microphones (inputs), as the engine lists them. */
    val microphones get() = local.sources.filter { it.isInput }

    fun chooseMicrophone(id: String?) {
        microphone = id
        store.settings.edit().putString("microphone", id).apply()
        val name = microphones.firstOrNull { it.id == id }?.name ?: "the phone's default microphone"
        announce("Microphone: $name.")
    }

    // MARK: Devices

    fun accountOf(d: Device): String? =
        devicesByAccount.entries.firstOrNull { (_, list) -> list.any { it.nodeId == d.nodeId } }?.key

    /** Keeps a device's choices valid: its default sound and output, and this phone's output and microphone. */
    fun fillChoices(d: Device) {
        var c = choices[d.nodeId] ?: DeviceChoices()
        if (d.sources.none { it.id == c.listenSource }) c = c.copy(listenSource = (d.sources.firstOrNull { it.isDefault } ?: d.sources.firstOrNull())?.id)
        if (d.outputs.none { it.id == c.sendOutput }) c = c.copy(sendOutput = (d.outputs.firstOrNull { it.isDefault } ?: d.outputs.firstOrNull())?.id)
        if (local.outputs.none { it.id == c.listenOutput }) c = c.copy(listenOutput = (local.outputs.firstOrNull { it.isDefault } ?: local.outputs.firstOrNull())?.id)
        choices[d.nodeId] = c
    }

    fun choose(nodeId: String, change: (DeviceChoices) -> DeviceChoices) {
        choices[nodeId] = change(choices[nodeId] ?: DeviceChoices())
    }

    fun toggleExpanded(nodeId: String) {
        if (expanded.contains(nodeId)) expanded.remove(nodeId) else expanded.add(nodeId)
    }

    fun listen(d: Device) {
        val (account, client) = connection(d) ?: return
        if (!d.online) return announce("${d.name} is offline.")
        if (!d.sharing) return announce("${d.name} is not sharing its audio, so it cannot be listened to. You can still send to it.")
        fillChoices(d)
        val ch = choices[d.nodeId] ?: DeviceChoices()
        val s = d.sources.firstOrNull { it.id == ch.listenSource } ?: return announce("Choose the sound to listen to.")
        val o = local.outputs.firstOrNull { it.id == ch.listenOutput } ?: return announce("This phone has no output to play on.")
        start(account, "Listening to ${s.name} on ${d.name}", d, listening = true) { client.listen(d.nodeId, s.id, o.id) }
    }

    fun send(d: Device) {
        val (account, client) = connection(d) ?: return
        if (!d.online) return announce("${d.name} is offline.")
        if (!sharingAccounts.contains(account)) return announce("This phone is not sharing its audio in ${name(account)}. Turn on sharing there to send from it.")
        if (!microphoneAllowed) return announce("Microphone access is off for AudioNet. Allow it when asked, or in Settings, Apps, AudioNet, Permissions.")
        fillChoices(d)
        val ch = choices[d.nodeId] ?: DeviceChoices()
        val mic = microphones.firstOrNull { it.id == microphone } ?: microphones.firstOrNull { it.isDefault } ?: microphones.firstOrNull()
            ?: return announce("This phone's microphone is not available.")
        val o = d.outputs.firstOrNull { it.id == ch.sendOutput } ?: return announce("Choose where it should play.")
        start(account, "Sending this phone's microphone to ${o.name} on ${d.name}", d, listening = false) { client.send(d.nodeId, mic.id, o.id) }
    }

    private fun connection(d: Device): Pair<String, Client>? {
        val account = accountOf(d) ?: run { announce("${d.name} is no longer listed in your accounts."); return null }
        val client = clients[account] ?: run { announce("Not connected to ${name(account)} yet. Try again in a moment."); return null }
        return account to client
    }

    private fun start(account: String, title: String, d: Device, listening: Boolean, begin: () -> String) {
        try {
            val id = begin()
            streams.add(StreamRow(id, account, title, d.nodeId, d.name, listening, "starting"))
            announce("Starting: $title.")
        } catch (e: Exception) {
            announce("Could not start: ${describe(e)}")
        }
    }

    // MARK: Sharing and streams

    fun isSharing(account: String) = sharingAccounts.contains(account)

    fun setSharing(account: String, on: Boolean) {
        if (accounts.none { it.nodeId == account }) return
        store.settings.edit().putBoolean("sharing-$account", on).apply()
        if (on) { if (!sharingAccounts.contains(account)) sharingAccounts.add(account) } else sharingAccounts.remove(account)
        clients[account]?.setSharing(on)
        announce(if (on) "Sharing this phone's audio in ${name(account)}: your devices there can listen to it, and it can send its audio."
                 else "Stopped sharing in ${name(account)}. This phone is still online there: it receives audio, but sends none of its own.")
    }

    fun stopStream(row: StreamRow) {
        clients[row.account]?.stopStream(row.id)
    }

    fun setVolume(id: String, volume: Float? = null, muted: Boolean? = null) {
        val i = streams.indexOfFirst { it.id == id }
        if (i < 0) return
        val row = streams[i].copy(volume = volume ?: streams[i].volume, muted = muted ?: streams[i].muted)
        streams[i] = row
        clients[row.account]?.setStreamVolume(id, row.volume, row.muted)
    }

    fun name(account: String): String = accounts.firstOrNull { it.nodeId == account }?.accountName ?: "an account"

    // MARK: Engine events

    private fun handle(event: Event, account: String) {
        when (event) {
            is Event.Connected -> {
                if (!connectedAccounts.contains(account)) connectedAccounts.add(account)
                val how = if (isSharing(account)) "sharing its audio" else "not sharing its audio"
                announce(if (accounts.size > 1) "Online in ${name(account)}, $how." else "Online, $how.")
                refreshLocalAudio()
            }
            is Event.Disconnected -> {
                connectedAccounts.remove(account)
                note("Disconnected from ${name(account)}: ${event.reason}. Reconnecting.")
            }
            is Event.Status -> note(event.text)
            is Event.Devices -> {
                devicesByAccount[account] = event.devices
                if (!devicesLoaded.contains(account)) devicesLoaded.add(account)
                event.devices.forEach { fillChoices(it) }
                expanded.retainAll(devices.map { it.nodeId }.toSet())
            }
            is Event.DeviceChanged -> {
                val list = devicesByAccount[account].orEmpty().toMutableList()
                val i = list.indexOfFirst { it.nodeId == event.device.nodeId }
                if (i >= 0) {
                    val was = list[i]
                    list[i] = event.device
                    if (was.online != event.device.online) {
                        announce("${event.device.name} is now ${if (event.device.online) "online" else "offline"}.")
                    } else if (event.device.online && was.sharing != event.device.sharing) {
                        announce("${event.device.name} ${if (event.device.sharing) "started" else "stopped"} sharing its audio.")
                    }
                } else {
                    list.add(event.device)
                }
                devicesByAccount[account] = list
                fillChoices(event.device)
            }
            is Event.Stream -> {
                val i = streams.indexOfFirst { it.id == event.sessionId }
                if (i < 0) return
                val text = if (event.state == StreamState.ACTIVE) "connected" else event.detail.trim('.').lowercase()
                val first = event.state == StreamState.ACTIVE && streams[i].state != "connected"
                streams[i] = streams[i].copy(state = text)
                if (first) announce("${streams[i].title}: connected.")
                if (event.detail.startsWith("Warning:")) announce("${streams[i].title}: ${event.detail}")
            }
            is Event.StreamEnded -> {
                val i = streams.indexOfFirst { it.id == event.sessionId }
                if (i >= 0) {
                    val row = streams.removeAt(i)
                    diagnostics.remove(row.id)
                    announce("Stopped: ${row.title}. ${event.reason}")
                }
            }
            is Event.Diagnostics -> {
                if (streams.any { it.id == event.sessionId }) diagnostics[event.sessionId] = event.text
            }
            is Event.ServerProblem -> announce(event.message)
            is Event.Stopped -> {
                clients.remove(account)
                disconnect(account)
                event.error?.let { announce("AudioNet stopped in ${name(account)}: $it") }
                if (clients.isEmpty() && online) clearOnlineState()
            }
        }
        OnlineService.update(context, this)
    }

    /** The engine's stream counts: the notification and the wake locks follow them. */
    private val audioUse = object : AudioUseListener {
        override fun audioInUse(streams: UInt, microphones: UInt) {
            main.post {
                engineStreams = streams.toInt()
                OnlineService.streamsChanged(context, streams.toInt(), microphones.toInt())
                OnlineService.update(context, this@AppModel)
            }
        }
    }

    companion object {
        @Volatile private var instance: AppModel? = null

        fun get(context: Context): AppModel =
            instance ?: synchronized(this) {
                instance ?: AppModel(context.applicationContext).also { instance = it }
            }
    }
}

/** An error in words. */
fun describe(e: Throwable): String = when (e) {
    is AudioNetException.Failed -> e.message ?: "unknown error"
    else -> e.message ?: e.javaClass.simpleName
}
