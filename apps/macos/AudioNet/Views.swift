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
    @Environment(\.openURL) private var openURL
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
                // Passwords are reset in the server's web client, which
                // emails a link to the account's address.
                Button("Forgot Password?", action: forgotPassword)
                    .accessibilityIdentifier("forgotPassword")
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

    private func forgotPassword() {
        var base = server.trimmingCharacters(in: .whitespaces)
        while base.hasSuffix("/") { base.removeLast() }
        guard base.hasPrefix("https://") || base.hasPrefix("http://"),
              let url = URL(string: base + "/?forgot") else {
            model.announce("Enter the server address first, starting with https://.")
            focus = .server
            return
        }
        openURL(url)
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

/// The signed-in window: three tabs (Devices, Streams, Accounts), and
/// under them a line saying how this Mac is doing and the status log.
/// Settings are in AudioNet's Settings window (Command-comma), as usual
/// on the Mac.
struct MainView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            TabView(selection: $model.tab) {
                DevicesTab()
                    .tabItem { Text("Devices") }
                    .tag(MainTab.devices)
                StreamsTab()
                    .tabItem { Text(model.streams.isEmpty ? "Streams" : "Streams (\(model.streams.count))") }
                    .tag(MainTab.streams)
                AccountsTab()
                    .tabItem { Text("Accounts") }
                    .tag(MainTab.accounts)
            }
            .padding([.top, .horizontal], 12)
            HStack {
                // The row states its own label and value: LabeledContent's
                // accessibility value stays at what it was at launch on
                // macOS.
                Text(statusText)
                    .accessibilityLabel("Status")
                    .accessibilityValue(statusText)
                    .accessibilityAddTraits(.isStaticText)
                    .accessibilityIdentifier("status")
                Spacer()
                StatusLogButton()
            }
            .padding(12)
        }
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

/// The account's visible devices, each a disclosure with Listen and Send.
struct DevicesTab: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Form {
            // One section per account with visible devices (a heading names
            // the account when there are several; nothing else repeats it).
            // An account with none has no section at all.
            ForEach(model.accounts.filter(showsSection), id: \.nodeId) { a in
                Section {
                    let list = model.devicesByAccount[a.nodeId] ?? []
                    if !model.connectedAccounts.contains(a.nodeId) {
                        Text("Connecting…")
                    } else if !model.devicesLoaded.contains(a.nodeId) {
                        Text("Looking for your devices…")
                    } else {
                        // One native disclosure per device: a single line
                        // until expanded. VoiceOver reads "Studio PC,
                        // Windows, collapsed, disclosure triangle"; VO-Space
                        // or the arrow keys open it.
                        ForEach(list, id: \.nodeId) { d in
                            DisclosureGroup(isExpanded: model.expansion(d.nodeId)) {
                                DeviceControls(device: d)
                            } label: {
                                // The whole line toggles, not just the arrow.
                                Text(verbatim: deviceLabel(d))
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
                        .accessibilityAddTraits(.isHeader)
                }
            }
            if model.accounts.allSatisfy({ !showsSection($0) }) {
                Section {
                    Text("No other devices are visible. Devices appear here while AudioNet runs on them with visibility on.")
                }
            }
        }
        .formStyle(.grouped)
    }

    /// An account's section shows while it connects and looks, and then
    /// only when it has visible devices.
    private func showsSection(_ a: Account) -> Bool {
        let id = a.nodeId
        return !model.connectedAccounts.contains(id) || !model.devicesLoaded.contains(id)
            || !(model.devicesByAccount[id] ?? []).isEmpty
    }
}

/// The streams this Mac started: state, stop, mute and volume.
struct StreamsTab: View {
    @EnvironmentObject var model: AppModel
    /// The selected stream (the newest, until another is chosen).
    @State private var selection: StreamRow.ID?

    private var selected: StreamRow? {
        model.streams.first { $0.id == selection }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Streams this Mac started").font(.headline)
                .accessibilityAddTraits(.isHeader)
            if model.streams.isEmpty {
                Text("No streams are running. Start one from Devices: open a device, then Listen or Send.")
                    .accessibilityIdentifier("noStreams")
                Spacer()
            } else {
                // A standard table, like the Windows app's list: one row per
                // stream, and one set of controls for the selected one.
                StreamTable(rows: model.streams.map { [$0.title, $0.state, model.volumeText($0)] },
                             ids: model.streams.map(\.id), selection: $selection)
                .frame(minHeight: 160)
                if let row = selected {
                    SelectedStreamControls(row: row)
                }
            }
        }
        .padding(16)
        .onAppear(perform: keepSelection)
        .onChange(of: model.streams.map(\.id)) { keepSelection() }
    }

    /// Something is always selected while streams run: the chosen one, or
    /// else the newest.
    private func keepSelection() {
        if selection == nil || !model.streams.contains(where: { $0.id == selection }) {
            selection = model.streams.last?.id
        }
    }
}

/// The streams table: a standard AppKit table (NSTableView) with Stream,
/// State and Volume columns and one selected row, like the Windows app's
/// list. Its cells are plain labels, which VoiceOver reads as the row
/// ("Listening to …, connected, 100 percent") and the column headers name.
/// Rows are updated in place, so the selection and VoiceOver's place stay.
struct StreamTable: NSViewRepresentable {
    let rows: [[String]]
    let ids: [String]
    @Binding var selection: String?

    static let columns = [("Stream", "stream", 300.0), ("State", "streamState", 140.0), ("Volume", "streamVolumeText", 150.0)]

    @MainActor final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var parent: StreamTable
        var rows: [[String]] = []
        var ids: [String] = []
        var updating = false
        init(_ parent: StreamTable) { self.parent = parent }

        func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let column = tableColumn, let c = tableView.tableColumns.firstIndex(of: column), row < rows.count else { return nil }
            let id = NSUserInterfaceItemIdentifier(StreamTable.columns[c].1)
            let cell = tableView.makeView(withIdentifier: id, owner: nil) as? NSTableCellView ?? {
                let v = NSTableCellView()
                v.identifier = id
                let label = NSTextField(labelWithString: "")
                label.translatesAutoresizingMaskIntoConstraints = false
                label.lineBreakMode = .byTruncatingTail
                label.setAccessibilityIdentifier(id.rawValue)
                v.addSubview(label)
                v.textField = label
                NSLayoutConstraint.activate([
                    label.leadingAnchor.constraint(equalTo: v.leadingAnchor, constant: 4),
                    label.trailingAnchor.constraint(equalTo: v.trailingAnchor, constant: -4),
                    label.centerYAnchor.constraint(equalTo: v.centerYAnchor),
                ])
                return v
            }()
            cell.textField?.stringValue = rows[row][c]
            return cell
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table = notification.object as? NSTableView else { return }
            let r = table.selectedRow
            parent.selection = r >= 0 && r < ids.count ? ids[r] : nil
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let table = NSTableView()
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        for (title, id, width) in Self.columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
            column.title = title
            column.width = width
            table.addTableColumn(column)
        }
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.setAccessibilityLabel("Streams this Mac started")
        table.setAccessibilityIdentifier("streams")
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? NSTableView else { return }
        let c = context.coordinator
        c.parent = self
        c.updating = true
        defer { c.updating = false }
        if c.ids != ids {
            c.rows = rows
            c.ids = ids
            table.reloadData()
        } else if c.rows != rows {
            // Same streams, new words (state, volume): those rows in place.
            let changed = IndexSet(rows.indices.filter { rows[$0] != c.rows[$0] })
            c.rows = rows
            table.reloadData(forRowIndexes: changed, columnIndexes: IndexSet(integersIn: 0..<Self.columns.count))
        }
        let want = selection.flatMap { ids.firstIndex(of: $0) } ?? -1
        if table.selectedRow != want {
            if want >= 0 {
                table.selectRowIndexes(IndexSet(integer: want), byExtendingSelection: false)
            } else {
                table.deselectAll(nil)
            }
        }
    }
}

/// Mute, volume and Stop for the stream selected in the table (their
/// names say so; the table row says which stream), and its measurements
/// when "Show Measurements" is on.
struct SelectedStreamControls: View {
    @EnvironmentObject var model: AppModel
    let row: StreamRow

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Toggle("Mute the Selected Stream", isOn: model.muted(row.id))
                    .accessibilityIdentifier("streamMute")
                Spacer()
                Button("Stop the Selected Stream") { model.stopStream(row) }
                    .accessibilityIdentifier("stopStream")
            }
            LabeledContent("Volume of the Selected Stream") {
                // No visible percentage: the table says it, and the
                // slider's spoken value says it.
                Slider(value: model.volume(row.id), in: 0...1, step: 0.05)
                    .accessibilityLabel("Volume of the Selected Stream")
                    .accessibilityValue(model.volumeText(row))
                    .accessibilityIdentifier("streamVolume")
            }
            // Measurements only when "Show Measurements" is on (Settings);
            // never announced.
            if model.showMeasurements, let live = model.diagnostics[row.id] {
                LiveTextRow(live: live)
                    .font(.callout)
                    .accessibilityIdentifier("streamDiagnostics")
            }
        }
    }
}

/// The accounts this Mac is signed in to: its visibility in each, Sign
/// Out, Add Account, and the web client's Manage Devices.
struct AccountsTab: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.openURL) private var openURL
    @State private var addingAccount = false
    /// The account Sign Out was chosen for (asked first).
    @State private var signingOut: Account?

    private var signOutQuestion: String {
        "Sign out of \(signingOut?.accountName ?? "this account")? This Mac is removed from that account. To use it there again, sign in again."
    }

    var body: some View {
        Form {
            ForEach(model.accounts, id: \.nodeId) { a in
                Section {
                    // Verbatim: an interpolated string would turn the
                    // address into a link, drawn in a lower-contrast color.
                    // The heading names the account; nothing else repeats it.
                    Text(verbatim: "This Mac's name here: \(a.deviceName)")
                    Toggle("Visible to My Other Devices", isOn: model.visibility(a.nodeId))
                        .disabled(model.signingOut.contains(a.nodeId))
                        // Named on the switch itself: in a grouped form on
                        // macOS the switch otherwise has no name of its own.
                        .accessibilityLabel("Visible to My Other Devices")
                        .accessibilityIdentifier("visible")
                    Text(model.isVisible(a.nodeId)
                         ? "Your other devices see this Mac, and can listen to it and send to it."
                         : "Your other devices cannot see this Mac or reach it. It can still listen to them and send to them.")
                    HStack {
                        Button("Manage Devices…") {
                            if let url = URL(string: a.serverUrl.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/?manage") {
                                openURL(url)
                            }
                        }
                        .accessibilityIdentifier("manageDevices")
                        Spacer()
                        Button(model.signingOut.contains(a.nodeId) ? "Signing Out…" : "Sign Out…") { signingOut = a }
                            .disabled(model.signingOut.contains(a.nodeId))
                            .accessibilityIdentifier("signOut")
                    }
                } header: {
                    // Verbatim: the server address is not a link.
                    Text(verbatim: a.accountName).font(.headline)
                        .accessibilityAddTraits(.isHeader)
                }
            }
            Section {
                Button("Add Account…") { addingAccount = true }
                    .accessibilityIdentifier("addAccount")
                Text("Renaming and removing your devices is in Manage Devices, in the web client.")
            }
        }
        .formStyle(.grouped)
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
        Button("Listen") { model.listen(device) }
            .disabled(!model.online)
            .accessibilityLabel("Listen to \(device.name)")
            .accessibilityIdentifier("listen")
        PopUpPicker(label: "Send from this Mac", identifier: "sendSource",
                    items: model.local.sources.map { ($0.id, sourceLabel($0)) }, selection: model.choice(id, \.sendSource))
        PopUpPicker(label: "To its output", identifier: "sendOutput",
                    items: device.outputs.map { ($0.id, outputLabel($0)) }, selection: model.choice(id, \.sendOutput))
        Button("Send") { model.send(device) }
            .disabled(!model.online)
            .accessibilityLabel("Send to \(device.name)")
            .accessibilityIdentifier("send")
    }
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

/// "Status Log…": opens the status log in a dialog.
struct StatusLogButton: View {
    @State private var showing = false

    var body: some View {
        Button("Status Log…") { showing = true }
            .accessibilityIdentifier("openStatusLog")
            .sheet(isPresented: $showing) { StatusLogDialog() }
    }
}

/// A form section with "Status Log…" (the sign-in page's).
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
            Toggle("Mute this Mac's sound while it is streamed to another device", isOn: $model.muteStreamedOutput)
                .accessibilityIdentifier("muteStreamedOutput")
            Text("While another device listens to one of this Mac's outputs, such as its speakers, that output stays silent until the stream stops, including VoiceOver, which you then hear on the other device. AudioNet's own sound and microphones are not affected, and the volume and mute in Control Center are not changed.")
                .font(.callout)
                .foregroundStyle(.secondary)
            // What the mute is doing now, in words, read again every
            // second while Settings is open.
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                LabeledContent("Muting now", value: outputMuteStatus())
            }
            Toggle("Show measurements under streams and in the status log (for troubleshooting)",
                   isOn: $model.showMeasurements)
                .accessibilityIdentifier("showMeasurements")
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
