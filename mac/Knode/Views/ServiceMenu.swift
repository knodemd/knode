import AppKit
import SwiftUI

/// The menu of the Knode menu bar item: whether Knode Service runs, what it has open, the space
/// agents and capture write to, and the actions to open the app, start or stop the service and
/// start it at login.
struct ServiceMenu: View {
    let monitor: ServiceMonitor
    let model: AppModel
    let searchIndex: SearchIndexModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        status
        if let error = monitor.errorMessage {
            Text(error)
        }
        Divider()
        spaceForAgents
        SettingsLink {
            Label("Agents and Access…", systemImage: "lock.shield")
        }
        Divider()
        Button("Open Knode", systemImage: "macwindow") {
            openWindow(id: "main")
            NSApp.activate()
        }
        if monitor.isRunning {
            Button("Stop Knode Service", systemImage: "stop.circle") {
                Task { await monitor.stopService() }
            }
            .disabled(monitor.isBusy)
        } else {
            Button("Start Knode Service", systemImage: "play.circle") {
                Task { await monitor.startService() }
            }
            .disabled(monitor.isBusy)
        }
        Toggle("Run at Login", systemImage: "power.circle", isOn: Binding(
            get: { monitor.runsAtLogin },
            set: { on in Task { await monitor.setRunsAtLogin(on) } }
        ))
        .disabled(monitor.isBusy)
        Divider()
        Button("Quit Knode", systemImage: "xmark.circle") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }

    /// Which space agents and capture write to: `default_space` in `config.toml`, read again by
    /// the service and the `knode` command, so no restart is needed.
    private var spaceForAgents: some View {
        Menu("Space for Agents", systemImage: "sparkles") {
            let spaces = model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
            ForEach(spaces) { item in
                Toggle(isOn: Binding(
                    get: { item.isDefault },
                    set: { on in model.setDefaultSpace(on ? item.url : nil) }
                )) {
                    Text(item.icon.map { "\($0) \(item.name)" } ?? item.name)
                }
            }
            if spaces.isEmpty {
                Text("No spaces yet")
            }
        }
        .onAppear { model.reloadDefaultSpace() }
    }

    @ViewBuilder private var status: some View {
        switch monitor.state {
        case .checking:
            Text("Checking Knode Service…")
        case .stopped:
            Text("Knode Service is stopped")
        case .running(let status):
            Text("Knode Service is running")
            Text(Self.summary(of: status))
            if !status.spaces.isEmpty {
                Section("Open Spaces") {
                    ForEach(status.spaces, id: \.self) { path in
                        Text(Self.name(ofSpaceAt: path))
                    }
                }
            }
            if let index = status.index {
                SearchIndexMenu(status: index, index: searchIndex)
            }
        }
    }

    static func summary(of status: ServiceMonitor.Status) -> String {
        let spaces = status.spaces.count == 1 ? "1 space" : "\(status.spaces.count) spaces"
        let clients = status.clients == 1 ? "1 client" : "\(status.clients) clients"
        return "\(spaces) open, \(clients) connected"
    }

    static func name(ofSpaceAt path: String) -> String {
        URL(filePath: path).deletingPathExtension().lastPathComponent
    }
}
