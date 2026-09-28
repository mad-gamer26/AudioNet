import AppKit
import SwiftUI

/// Sign-in until this Mac belongs to an account, then the main view.
struct RootView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Group {
            if model.readingAccount {
                Form {
                    Text("Reading this Mac's AudioNet sign-in…")
                    LogSection()
                }
                .formStyle(.grouped)
            } else if !model.signedIn {
                SignInView()
            } else {
                MainView()
            }
        }
        .background(WindowContentName(name: "AudioNet"))
    }
}

/// Names the window's content view (the AppKit container SwiftUI draws
/// into), which otherwise reaches VoiceOver as an unnamed group.
struct WindowContentName: NSViewRepresentable {
    let name: String

    final class Probe: NSView {
        var name = ""
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            window?.contentView?.setAccessibilityLabel(name)
        }
    }

    func makeNSView(context: Context) -> Probe {
        let v = Probe()
        v.name = name
        v.setAccessibilityElement(false)
        return v
    }

    func updateNSView(_ v: Probe, context: Context) {}
}

/// Signs this Mac in to an account: the first one, or (`adding`, in a sheet
/// from Add Account…) another one; this Mac is then a device in each.
struct SignInView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var adding = false
    @State private var server = ""
    @State private var username = ""
    @State private var password = ""
    @State private var deviceName = Host.current().localizedName ?? "Mac"
    @FocusState private var focus: Field?

    enum Field { case server, username, password, deviceName }

    var body: some View {
        Form {
            Section {
                TextField("Server address", text: $server, prompt: Text("https://audionet.example.com"))
                    .textContentType(.URL)
                    .focused($focus, equals: .server)
                    .accessibilityIdentifier("server")
                TextField("Account name", text: $username)
                    .textContentType(.username)
                    .focused($focus, equals: .username)
                    .accessibilityIdentifier("username")
                SecureField("Password", text: $password)
                    .textContentType(.password)
                    .focused($focus, equals: .password)
                    .accessibilityIdentifier("password")
                TextField("Name for this Mac", text: $deviceName)
                    .focused($focus, equals: .deviceName)
                    .accessibilityIdentifier("deviceName")
                Button(model.signingIn ? "Signing In…" : "Sign In", action: submit)
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.signingIn)
                    .accessibilityIdentifier("signIn")
                if adding {
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                }
                // A row, not the section footer: macOS draws footers in a
                // light grey that fails WCAG AA.
                Text(adding
                     ? "Use the other AudioNet account. This Mac becomes a device there too; the password is used once and is not stored."
                     : "Use your AudioNet account. The password is used once to add this Mac and is not stored.")
            } header: {
                Text(adding ? "Add an account" : "Sign in to AudioNet").font(.headline)
            }
            if !adding { LogSection() }
        }
        .formStyle(.grouped)
        .frame(minWidth: adding ? 480 : nil)
        .onAppear {
            if server.isEmpty { server = model.defaultServer }
            focus = server.isEmpty ? .server : .username
        }
    }

    private func submit() {
        let fields: [(String, Field)] = [(server, .server), (username, .username), (password, .password), (deviceName, .deviceName)]
        if let missing = fields.first(where: { $0.0.trimmingCharacters(in: .whitespaces).isEmpty }) {
            model.announce("Enter the server address, your account name and password, and a name for this Mac.")
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

struct MainView: View {
    @EnvironmentObject var model: AppModel
    @State private var addingAccount = false
    /// The account Sign Out was chosen for (asked first).
    @State private var signingOut: Account?

    private var signOutQuestion: String {
        "Sign out of \(signingOut?.accountName ?? "this account")? This Mac is removed from that account and stops sharing there. To use it there again, sign in again."
    }

    var body: some View {
        Form {
            Section {
                // The row states its own label and value: LabeledContent's
                // accessibility value stays at what it was at launch on
                // macOS (VoiceOver kept reading "Offline" while the text on
                // screen said "Online").
                LabeledContent("Status") { Text(statusText).foregroundStyle(.primary) }
                    .accessibilityElement(children: .ignore)
                    .accessibilityAddTraits(.isStaticText)
                    .accessibilityLabel("Status")
                    .accessibilityValue(statusText)
                    .accessibilityIdentifier("status")
                // Sharing, per account: online either way while AudioNet
                // runs; sharing decides whether this Mac sends its audio.
                ForEach(model.accounts, id: \.nodeId) { a in
                    let label = model.accounts.count > 1 ? "Share This Mac's Audio in \(a.accountName)" : "Share This Mac's Audio"
                    Toggle(label, isOn: model.sharing(a.nodeId))
                        .disabled(model.signingOut.contains(a.nodeId))
                        // Named on the switch itself: in a grouped form on
                        // macOS the switch otherwise has no name of its own
                        // (VoiceOver would say only "switch").
                        .accessibilityLabel(label)
                        .accessibilityIdentifier("sharing")
                }
            } header: {
                Text("This Mac").font(.headline)
            }

            // Every account this Mac is signed in to, each with its own Sign
            // Out, and another one can be added.
            Section {
                ForEach(model.accounts, id: \.nodeId) { a in
                    HStack {
                        // Verbatim: an interpolated string would turn the
                        // address into a link, drawn in a lower-contrast color.
                        Text(verbatim: "Signed in as \"\(a.deviceName)\" to account \(a.username) on \(a.serverUrl).")
                        Spacer()
                        Button(model.signingOut.contains(a.nodeId) ? "Signing Out…" : "Sign Out") { signingOut = a }
                            .disabled(model.signingOut.contains(a.nodeId))
                            .accessibilityLabel("Sign Out of \(a.accountName)")
                            .accessibilityIdentifier("signOut")
                    }
                }
                Button("Add Account…") { addingAccount = true }
                    .accessibilityIdentifier("addAccount")
            } header: {
                Text("Accounts").font(.headline)
            }
            .sheet(isPresented: $addingAccount) { SignInView(adding: true) }
            .confirmationDialog(signOutQuestion, isPresented: Binding(
                get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }), titleVisibility: .visible) {
                Button("Sign Out", role: .destructive) {
                    if let a = signingOut { model.signOut(a) }
                    signingOut = nil
                }
                .accessibilityIdentifier("confirmSignOut")
                Button("Cancel", role: .cancel) { signingOut = nil }
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
                    Text("No other devices yet.")
                } else {
                    // One native disclosure per device: a single line until
                    // expanded. VoiceOver reads "Studio PC: online, collapsed,
                    // disclosure triangle"; VO-Space or the arrow keys open it.
                    ForEach(list, id: \.nodeId) { d in
                        DisclosureGroup(isExpanded: model.expansion(d.nodeId)) {
                            DeviceControls(device: d)
                        } label: {
                            // The whole line toggles, not just the arrow.
                            Text(verbatim: "\(d.name): \(deviceState(d))")
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .contentShape(Rectangle())
                                .onTapGesture { model.expansion(d.nodeId).wrappedValue.toggle() }
                                // The same for VoiceOver and Full Keyboard Access.
                                .accessibilityAction { model.expansion(d.nodeId).wrappedValue.toggle() }
                        }
                    }
                }
            } header: {
                Text(model.accounts.count > 1 ? "Devices in \(a.accountName)" : "Your devices").font(.headline)
            }
            }

            Section {
                if model.streams.isEmpty {
                    Text("No streams are running.")
                } else {
                    ForEach(model.streams) { row in
                        HStack {
                            VStack(alignment: .leading) {
                                Text("\(row.title): \(row.state)")
                                    .accessibilityIdentifier("stream")
                                // Measurements, readable on demand; never announced.
                                if let live = model.diagnostics[row.id] {
                                    LiveTextRow(live: live)
                                        .font(.callout)
                                        .foregroundStyle(.primary)
                                        .accessibilityIdentifier("streamDiagnostics")
                                }
                            }
                            Spacer()
                            Button("Stop") { model.stopStream(row) }
                                .accessibilityLabel("Stop \(row.title)")
                        }
                        StreamVolume(row: row)
                    }
                }
            } header: {
                Text("Streams this Mac started").font(.headline)
            }

            LogSection()
        }
        .formStyle(.grouped)
    }

    private var statusText: String {
        if model.accounts.isEmpty { return "Not signed in" }
        if !model.connected { return "Connecting…" }
        let sharing = model.sharingAccounts.count
        if model.accounts.count == 1 {
            return sharing == 1 ? "Online, sharing its audio" : "Online, not sharing its audio"
        }
        return "Online in \(model.accounts.count) accounts, sharing in \(sharing)"
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
        // spoken value says it.
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
        PopUpPicker(label: "Sound to listen to", identifier: "listenSource",
                    items: device.sources.map { ($0.id, sourceLabel($0)) }, selection: model.choice(id, \.listenSource))
        PopUpPicker(label: "Play it on", identifier: "listenOutput",
                    items: model.local.outputs.map { ($0.id, outputLabel($0)) }, selection: model.choice(id, \.listenOutput))
        if device.online && !device.sharing {
            Text("\(device.name) is not sharing its audio: it cannot be listened to, but you can send to it.")
        }
        Button("Listen") { model.listen(device) }
            .disabled(!model.online || !device.sharing)
            .accessibilityLabel("Listen to \(device.name)")
            .accessibilityIdentifier("listen")
        PopUpPicker(label: "Send from this Mac", identifier: "sendSource",
                    items: model.local.sources.map { ($0.id, sourceLabel($0)) }, selection: model.choice(id, \.sendSource))
        PopUpPicker(label: "To its output", identifier: "sendOutput",
                    items: device.outputs.map { ($0.id, outputLabel($0)) }, selection: model.choice(id, \.sendOutput))
        let sharingHere = model.account(of: device).map(model.isSharing) ?? false
        if !sharingHere {
            Text("Turn on Share This Mac's Audio to send from this Mac.")
        }
        Button("Send") { model.send(device) }
            .disabled(!model.online || !device.online || !sharingHere)
            .accessibilityLabel("Send to \(device.name)")
            .accessibilityIdentifier("send")
    }
}

/// A device's state in words: offline, online, or online but not sharing.
func deviceState(_ d: Device) -> String {
    !d.online ? "offline" : d.sharing ? "online" : "online, not sharing"
}

func sourceLabel(_ s: Source) -> String {
    "\(s.name) (\(s.isInput ? "input" : "what it plays")\(s.isDefault ? ", default" : ""))"
}

func outputLabel(_ o: Output) -> String {
    o.isDefault ? "\(o.name) (default)" : o.name
}

/// A labeled pop-up menu row. AppKit's NSPopUpButton, not a SwiftUI
/// Picker: it offers the standard press action (VoiceOver: VO-Space opens
/// it), which SwiftUI's pop-up picker does not report to the macOS 27
/// accessibility audit.
struct PopUpPicker: View {
    let label: String
    let identifier: String
    let items: [(id: String, title: String)]
    @Binding var selection: String?

    var body: some View {
        LabeledContent(label) {
            PopUpButton(label: label, identifier: identifier, items: items, selection: $selection)
                .fixedSize()
        }
    }
}

struct PopUpButton: NSViewRepresentable {
    let label: String
    let identifier: String
    let items: [(id: String, title: String)]
    @Binding var selection: String?

    @MainActor final class Coordinator: NSObject {
        var parent: PopUpButton
        init(_ parent: PopUpButton) { self.parent = parent }
        @objc func changed(_ button: NSPopUpButton) {
            let i = button.indexOfSelectedItem
            parent.selection = parent.items.indices.contains(i) ? parent.items[i].id : nil
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSPopUpButton {
        let button = NSPopUpButton(frame: .zero, pullsDown: false)
        button.target = context.coordinator
        button.action = #selector(Coordinator.changed(_:))
        button.setAccessibilityLabel(label)
        button.setAccessibilityIdentifier(identifier)
        return button
    }

    func updateNSView(_ button: NSPopUpButton, context: Context) {
        context.coordinator.parent = self
        let titles = items.map(\.title)
        if button.itemTitles != titles {
            // Item by item: addItems(withTitles:) drops repeated titles.
            button.removeAllItems()
            for t in titles { button.menu?.addItem(withTitle: t, action: nil, keyEquivalent: "") }
        }
        if let i = items.firstIndex(where: { $0.id == selection }) {
            if button.indexOfSelectedItem != i { button.selectItem(at: i) }
        }
    }
}

/// A button that opens the status log in a dialog, so the log does not
/// fill the window.
struct LogSection: View {
    @EnvironmentObject var model: AppModel
    @State private var showing = false

    var body: some View {
        Section {
            Button("Status Log…") { showing = true }
                .accessibilityIdentifier("openStatusLog")
        }
        .sheet(isPresented: $showing) { StatusLogDialog() }
    }
}

/// The status log dialog: every event in words, in a read-only text area
/// that VoiceOver reads and moves through line by line (and that can be
/// selected and copied), like the Windows app's. Copy, and Close (also
/// Escape).
struct StatusLogDialog: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Status Log").font(.headline)
                .accessibilityAddTraits(.isHeader)
            LogTextView(text: model.log.joined(separator: "\n"))
                .frame(minWidth: 520, minHeight: 320)
            HStack {
                Button("Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(model.log.joined(separator: "\n"), forType: .string)
                    model.announce("Status copied.")
                }
                .accessibilityLabel("Copy Status Log")
                .accessibilityIdentifier("copyLog")
                Spacer()
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("closeLog")
            }
        }
        .padding(20)
    }
}

/// A scrolling, selectable, non-editable NSTextView named "Status log",
/// kept scrolled to the newest line.
struct LogTextView: NSViewRepresentable {
    let text: String

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        let view = scroll.documentView as! NSTextView
        view.isEditable = false
        view.isSelectable = true
        view.drawsBackground = false
        view.font = .preferredFont(forTextStyle: .body)
        view.textContainerInset = NSSize(width: 4, height: 4)
        view.setAccessibilityLabel("Status log")
        view.setAccessibilityIdentifier("statusLog")
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let view = scroll.documentView as! NSTextView
        guard view.string != text else { return }
        view.string = text
        view.scrollToEndOfDocument(nil)
    }
}

struct SettingsView: View {
    @EnvironmentObject var model: AppModel
    @ObservedObject var updater: Updater

    var body: some View {
        Form {
            Toggle("Start AudioNet when I log in", isOn: Binding(
                get: { model.launchAtLogin },
                set: { model.launchAtLogin = $0 }))
            Toggle("Keep AudioNet running in the menu bar when its window is closed (without a Dock icon)",
                   isOn: $model.keepInMenuBar)
                .onChange(of: model.keepInMenuBar) { if !model.keepInMenuBar { model.startInMenuBar = false } }
            Toggle("Start AudioNet in the menu bar, without its window or Dock icon", isOn: $model.startInMenuBar)
                .onChange(of: model.startInMenuBar) { if model.startInMenuBar { model.keepInMenuBar = true } }
            Section {
                Toggle("Keep AudioNet up to date automatically", isOn: $updater.automatic)
                    .disabled(!updater.configured)
                Button("Check for Updates Now") { Task { await updater.check(userAsked: true) } }
                    .disabled(!updater.configured)
                if !updater.status.isEmpty {
                    LabeledContent("Updates", value: updater.status)
                }
                LabeledContent("Version", value: Updater.currentVersion)
                LabeledContent("Engine version", value: engineVersion())
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
    }
}
