import SwiftUI
import UIKit

/// Sign-in until this iPhone belongs to an account, then the tabs.
struct RootView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Group {
            if model.signedIn && !model.readingAccount {
                MainTabs()
            } else {
                NavigationStack {
                    Group {
                        if model.readingAccount {
                            Form { Text("Reading this iPhone's AudioNet sign-in…") }
                        } else {
                            SignInView()
                        }
                    }
                    .navigationTitle("AudioNet")
                }
            }
        }
        .tint(accessibleTint)
    }
}

/// The signed-in screens: a standard tab bar (VoiceOver reads "Devices,
/// tab, 1 of 4") with Devices, Streams, Accounts and Settings, each with
/// its own navigation.
struct MainTabs: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        TabView(selection: $model.tab) {
            NavigationStack { DevicesScreen() }
                .tabItem { Label("Devices", systemImage: "hifispeaker.2") }
                .tag(MainTab.devices)
            NavigationStack { StreamsScreen() }
                .tabItem { Label("Streams", systemImage: "waveform") }
                .badge(model.streams.count)
                .tag(MainTab.streams)
            NavigationStack { AccountsScreen() }
                .tabItem { Label("Accounts", systemImage: "person.crop.circle") }
                .tag(MainTab.accounts)
            NavigationStack { SettingsView() }
                .tabItem { Label("Settings", systemImage: "gear") }
                .tag(MainTab.settings)
        }
    }
}

/// The accent color, at WCAG AA contrast (4.5:1 or more) on iOS's grouped
/// backgrounds in both appearances; the system blue falls just short.
let accessibleTint = Color(uiColor: UIColor { traits in
    traits.userInterfaceStyle == .dark
        ? UIColor(red: 0.45, green: 0.68, blue: 1.0, alpha: 1)    // #73ADFF
        : UIColor(red: 0.0, green: 0.33, blue: 0.75, alpha: 1)    // #0054BF
})

/// A section heading. Forms use prominent headers
/// (`.headerProminence(.increased)`): iOS draws those in the primary text
/// color, where the standard ones are a grey that falls short of WCAG AA.
func heading(_ text: String) -> some View {
    // Wraps rather than truncating at large text sizes.
    Text(text).fixedSize(horizontal: false, vertical: true)
}

/// Signs this iPhone in to an account: the first one, or (`adding`, from
/// Settings) another one; this iPhone is then a device in each.
struct SignInView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    var adding = false
    @State private var server = ""
    @State private var username = ""
    @State private var password = ""
    @State private var deviceName = UIDevice.current.name
    @FocusState private var focus: Field?

    enum Field { case server, username, password, deviceName }

    var body: some View {
        Form {
            Section {
                TextField("Server address", text: $server, prompt: Text("https://audionet.example.com"), axis: .vertical)
                    .lineLimit(1...4)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($focus, equals: .server)
                    .accessibilityIdentifier("server")
                TextField("Account name", text: $username, axis: .vertical)
                    .lineLimit(1...4)
                    .textContentType(.username)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($focus, equals: .username)
                    .accessibilityIdentifier("username")
                SecureField("Password", text: $password)
                    .textContentType(.password)
                    .focused($focus, equals: .password)
                    .accessibilityIdentifier("password")
                TextField("Name for this iPhone", text: $deviceName, axis: .vertical)
                    .lineLimit(1...4)
                    .focused($focus, equals: .deviceName)
                    .accessibilityIdentifier("deviceName")
                Button(model.signingIn ? "Signing In…" : "Sign In", action: submit)
                    .disabled(model.signingIn)
                    .accessibilityIdentifier("signIn")
                // Passwords are reset in the server's web client, which
                // emails a link to the account's address.
                Button("Forgot Password?", action: forgotPassword)
                    .accessibilityIdentifier("forgotPassword")
                if let problem = model.signInProblem {
                    Text(problem)
                        .accessibilityIdentifier("signInProblem")
                }
                Text(adding
                     ? "Use the other AudioNet account. This iPhone becomes a device there too; the password is used once and is not stored."
                     : "Use your AudioNet account. The password is used once to add this iPhone and is not stored.")
            } header: {
                heading(adding ? "Add an account" : "Sign in")
            }
            if !adding {
                Section {
                    StatusLogLink()
                }
            }
        }
        .navigationTitle(adding ? "Add Account" : "AudioNet")
        .headerProminence(.increased)
        .onSubmit(submit)
        .onAppear {
            if server.isEmpty { server = model.defaultServer }
        }
    }

    private func forgotPassword() {
        var base = server.trimmingCharacters(in: .whitespaces)
        while base.hasSuffix("/") { base.removeLast() }
        guard base.hasPrefix("https://") || base.hasPrefix("http://"),
              let url = URL(string: base + "/?forgot") else {
            model.reportSignInProblem("Enter the server address first, starting with https://.")
            focus = .server
            return
        }
        openURL(url)
    }

    private func submit() {
        let fields: [(String, Field)] = [(server, .server), (username, .username), (password, .password), (deviceName, .deviceName)]
        if let missing = fields.first(where: { $0.0.trimmingCharacters(in: .whitespaces).isEmpty }) {
            model.reportSignInProblem("Enter the server address, your account name and password, and a name for this iPhone.")
            focus = missing.1
            return
        }
        let pw = password
        password = "" // not kept
        model.signIn(server: server.trimmingCharacters(in: .whitespaces),
                     username: username.trimmingCharacters(in: .whitespaces),
                     password: pw,
                     deviceName: deviceName.trimmingCharacters(in: .whitespaces)) { ok in
            if ok && adding { dismiss() }
        }
    }
}

/// The account's visible devices, each a disclosure with Listen and Send;
/// above them, how this iPhone is doing and its microphone.
struct DevicesScreen: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Form {
            Section {
                // The row states its own label and value (on macOS the
                // same LabeledContent kept its launch-time value for
                // VoiceOver).
                LabeledContent("Status", value: statusText)
                    .accessibilityElement(children: .ignore)
                    .accessibilityAddTraits(.isStaticText)
                    .accessibilityLabel("Status")
                    .accessibilityValue(statusText)
                    .accessibilityIdentifier("status")
                MicrophoneRow(store: model.microphones)
            } header: {
                heading("This iPhone")
            }

            // One section per account (a heading names the account when
            // there are several).
            ForEach(model.accounts, id: \.nodeId) { a in
                Section {
                    let list = model.devicesByAccount[a.nodeId] ?? []
                    if !model.connectedAccounts.contains(a.nodeId) {
                        Text("Connecting…")
                    } else if !model.devicesLoaded.contains(a.nodeId) {
                        Text("Looking for your devices…")
                    } else if list.isEmpty {
                        Text("No other devices are visible. Devices appear here while AudioNet runs on them with visibility on.")
                    } else {
                        // One native disclosure per device: a single row
                        // until expanded ("Studio PC, Windows, collapsed").
                        ForEach(list, id: \.nodeId) { d in
                            DisclosureGroup(isExpanded: model.expansion(d.nodeId)) {
                                DeviceControls(device: d)
                            } label: {
                                Text(verbatim: deviceLabel(d))
                            }
                        }
                    }
                } header: {
                    heading(model.accounts.count > 1 ? "Devices in \(a.accountName)" : "Your devices")
                }
            }
        }
        .headerProminence(.increased)
        .navigationTitle("Devices")
    }

    private var statusText: String {
        if model.accounts.isEmpty { return "Not signed in" }
        if !model.connected { return "Connecting…" }
        let visible = model.visibleAccounts.count
        if model.accounts.count == 1 {
            return visible == 1 ? "Visible to your other devices" : "Hidden from your other devices"
        }
        return "Visible in \(visible) of \(model.accounts.count) accounts"
    }
}

/// The streams this iPhone started: state, mute, volume, stop.
struct StreamsScreen: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Form {
            if model.streams.isEmpty {
                Section {
                    Text("No streams are running. Start one from Devices: open a device, then Listen or Send.")
                }
            } else {
                // A section per stream; each part a row of its own, so
                // every row grows with the text size on its own.
                ForEach(model.streams) { row in
                    Section {
                        Text("\(row.title): \(row.state)")
                            .accessibilityIdentifier("stream")
                        // Measurements only when "Show Measurements" is on
                        // (Settings); never announced.
                        if model.showMeasurements, let live = model.diagnostics[row.id] {
                            LiveTextRow(live: live)
                        }
                        StreamVolume(row: row)
                        Button("Stop") { model.stopStream(row) }
                            .accessibilityLabel("Stop \(row.title)")
                    }
                }
            }
        }
        .headerProminence(.increased)
        .navigationTitle("Streams")
    }
}

/// The accounts this iPhone is signed in to: its visibility in each, Sign
/// Out, Manage Devices (in the web client), Add Account.
struct AccountsScreen: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.openURL) private var openURL
    /// The account Sign Out was chosen for (asked first).
    @State private var signingOut: Account?

    var body: some View {
        Form {
            ForEach(model.accounts, id: \.nodeId) { a in
                Section {
                    Text(verbatim: "Signed in as \"\(a.deviceName)\" to account \(a.username) on \(a.serverUrl).")
                    let label = model.accounts.count > 1 ? "Visible to My Other Devices in \(a.accountName)" : "Visible to My Other Devices"
                    // Short on screen (the section names the account); the
                    // spoken name says which account.
                    Toggle("Visible to My Other Devices", isOn: model.visibility(a.nodeId))
                        .disabled(model.signingOut.contains(a.nodeId))
                        .accessibilityLabel(label)
                        .accessibilityIdentifier("visible")
                    Text(model.isVisible(a.nodeId)
                         ? "Your other devices see this iPhone, and can listen to it and send to it."
                         : "Your other devices cannot see this iPhone or reach it. It can still listen to them and send to them.")
                    Button("Manage Devices") {
                        if let url = URL(string: a.serverUrl.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/?manage") {
                            openURL(url)
                        }
                    }
                    .accessibilityLabel(model.accounts.count > 1 ? "Manage Devices in \(a.accountName)" : "Manage Devices")
                    .accessibilityIdentifier("manageDevices")
                    Button(model.signingOut.contains(a.nodeId) ? "Signing Out…" : "Sign Out", role: .destructive) { signingOut = a }
                        .disabled(model.signingOut.contains(a.nodeId))
                        .accessibilityLabel("Sign Out of \(a.accountName)")
                        .accessibilityIdentifier("signOut")
                } header: {
                    heading(model.accounts.count > 1 ? a.accountName : "Account")
                }
            }
            Section {
                NavigationLink("Add Account") { SignInView(adding: true) }
                    .accessibilityIdentifier("addAccount")
                Text("Renaming and removing your devices is in Manage Devices, in the web client.")
            }
        }
        .headerProminence(.increased)
        .navigationTitle("Accounts")
        .confirmationDialog(
            "Sign out of \(signingOut?.accountName ?? "this account")? This iPhone is removed from that account. To use it there again, sign in again.",
            isPresented: Binding(get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }),
            titleVisibility: .visible) {
            Button("Sign Out", role: .destructive) {
                if let a = signingOut { model.signOut(a) }
                signingOut = nil
            }
            .accessibilityIdentifier("confirmSignOut")
            Button("Cancel", role: .cancel) { signingOut = nil }
        }
    }
}

/// A stream's mute switch and volume slider (on this device: what it plays
/// here, or what it sends). Their spoken names say which stream.
struct StreamVolume: View {
    @EnvironmentObject var model: AppModel
    let row: StreamRow

    var body: some View {
        let percent = Int((row.volume * 100).rounded())
        Toggle("Mute", isOn: model.muted(row.id))
            .accessibilityLabel("Mute \(row.title)")
            .accessibilityIdentifier("streamMute")
        // No visible percentage: the slider's position shows it, and its
        // spoken value says it (a separate text row was reported as clipped
        // at large text sizes).
        Slider(value: model.volume(row.id), in: 0...1, step: 0.05)
        .accessibilityLabel("Volume for \(row.title)")
        .accessibilityValue(row.muted ? "\(percent) percent, muted" : "\(percent) percent")
        .accessibilityIdentifier("streamVolume")
    }
}

/// Listen to and send to one device (inside its disclosure).
struct DeviceControls: View {
    @EnvironmentObject var model: AppModel
    let device: Device

    var body: some View {
        let id = device.nodeId
        Picker("Sound to listen to", selection: model.choice(id, \.listenSource)) {
            ForEach(device.sources, id: \.id) { s in Text(sourceLabel(s)).tag(Optional(s.id)) }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier("listenSource")
        Button("Listen") { model.listen(device) }
            .disabled(!model.online)
            .accessibilityLabel("Listen to \(device.name)")
            .accessibilityIdentifier("listen")
        Picker("Play my microphone on", selection: model.choice(id, \.sendOutput)) {
            ForEach(device.outputs, id: \.id) { o in Text(outputLabel(o)).tag(Optional(o.id)) }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier("sendOutput")
        Button("Send My Microphone") { model.send(device) }
            .disabled(!model.online)
            .accessibilityLabel("Send my microphone to \(device.name)")
            .accessibilityIdentifier("send")
    }
}

/// The Microphone row: says which microphone records, and opens the list
/// to choose another (like UniMic's Input Source).
struct MicrophoneRow: View {
    @ObservedObject var store: MicrophoneStore

    var body: some View {
        NavigationLink {
            MicrophoneList(store: store)
        } label: {
            LabeledContent("Microphone", value: store.selectedName)
        }
        .accessibilityIdentifier("microphone")
    }
}

/// Every microphone, a section per input (the iPhone's own microphone, a
/// wired headset; never Bluetooth), a row per source ("Bottom", "Front",
/// "Back"), the chosen one checked and marked selected. Choosing one keeps
/// the list open and, while the microphone is being sent, switches to it
/// at once.
/// Redraws only when the microphones change: log and stream updates never
/// move VoiceOver's place.
struct MicrophoneList: View {
    @EnvironmentObject var model: AppModel
    @ObservedObject var store: MicrophoneStore

    var body: some View {
        List {
            if store.groups.isEmpty {
                Text("Looking for microphones…")
            }
            ForEach(store.groups) { group in
                Section {
                    ForEach(group.microphones) { m in
                        let selected = store.selectedID == m.id
                        Button {
                            model.chooseMicrophone(m)
                        } label: {
                            HStack {
                                Text(m.name + (m.stereo ? ", stereo" : ""))
                                    .foregroundStyle(.primary)
                                Spacer()
                                if selected {
                                    Image(systemName: "checkmark").accessibilityHidden(true)
                                }
                            }
                        }
                        .accessibilityAddTraits(selected ? .isSelected : [])
                        .accessibilityIdentifier("microphoneChoice")
                    }
                } header: {
                    heading(group.name)
                }
            }
        }
        .headerProminence(.increased)
        .navigationTitle("Microphone")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { store.refresh() }
    }
}

/// Opens the status log.
struct StatusLogLink: View {
    var body: some View {
        NavigationLink("Status Log") { StatusLogView() }
            .accessibilityIdentifier("statusLog")
    }
}

/// The status log: every event in words, one row per line (VoiceOver moves
/// through them one by one), newest last, with Copy at the top right.
struct StatusLogView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        List {
            ForEach(Array(model.log.suffix(200).enumerated()), id: \.offset) { _, line in
                Text(line)
            }
        }
        .navigationTitle("Status Log")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Copy") {
                    UIPasteboard.general.string = model.log.joined(separator: "\n")
                    model.announce("Status copied.")
                }
                .accessibilityLabel("Copy Status Log")
                .accessibilityIdentifier("copyLog")
            }
        }
    }
}

struct SettingsView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var notifications: Notifications

    var body: some View {
        Form {
            Section {
                Toggle("Devices Becoming Visible or Hidden", isOn: $notifications.presence)
                    .accessibilityIdentifier("notifyPresence")
                Text("About the other devices in your accounts, titled with the account name. They do not appear while AudioNet is open.")
                if let problem = notifications.problem {
                    Text(problem)
                }
            } header: {
                heading("Notifications")
            }
            Section {
                Toggle("Show Measurements", isOn: $model.showMeasurements)
                    .accessibilityIdentifier("showMeasurements")
                Text("Numbers about streams and the network, under each stream and in the status log, for troubleshooting.")
                StatusLogLink()
            } header: {
                heading("Troubleshooting")
            }
            Section {
                LabeledContent("Version",
                               value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
                LabeledContent("Engine version", value: engineVersion())
            }
        }
        .headerProminence(.increased)
        .navigationTitle("Settings")
    }
}
