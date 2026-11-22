import Foundation
import Synchronization
import SwiftUI
import Testing
@testable import Knode

/// Connect your things against fakes: the tiles from `sources.list`, Full Disk Access noticed by
/// itself, how Gmail is connected in a build without Sign in with Google, the icons of web apps
/// and when the screen opens by itself. Nothing touches a real Knode home, another app's data or
/// the Applications folders.
@MainActor
@Suite(.serialized) struct ConnectSourcesTests {
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    static func connector(
        _ id: String, _ name: String, permissions: [SourcePermission] = [], state: SourceState = .off,
        connected: Bool = false, readiness: SourceReadiness? = nil
    ) -> SourceStatus {
        SourceStatus(
            id: id, name: name, kind: "connector", reads: "What \(name) has", permissions: permissions, container: "chat",
            state: connected && state == .off ? .idle : state,
            setting: connected ? SourceStatus.Setting(on: true, history: .fromToday, connectedAt: 1, since: 0) : nil,
            readiness: readiness
        )
    }

    static let noFullDiskAccess = SourceReadiness.blocked(kind: "permission", message: "Full Disk Access is needed")
    static let noGoogleClient = SourceReadiness.unsupported(message: "Sign in with Google is not set up in this build")

    // MARK: Readiness from the service

    @Test func readinessIsDecodedFromTheList() throws {
        let json = """
        [{ "id": "apple-mail", "name": "Apple Mail", "kind": "connector", "reads": "Mail", "state": "off",
           "readiness": { "state": "blocked", "kind": "permission", "message": "Full Disk Access is needed: give it" } },
         { "id": "gmail", "name": "Gmail", "state": "off", "readiness": { "state": "unsupported", "message": "no client id" } },
         { "id": "imap", "name": "Mail (IMAP)", "state": "off", "readiness": { "state": "ready" } },
         { "id": "slack", "name": "Slack", "state": "off", "readiness": { "state": "something-new" } },
         { "id": "old", "name": "Old", "state": "off" }]
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let list = try decoder.decode([SourceStatus].self, from: Data(json.utf8))
        #expect(list[0].readiness?.lacksFullDiskAccess == true)
        #expect(list[1].unsupportedReason == "no client id")
        #expect(list[2].readiness == .ready)
        #expect(list[3].readiness == .blocked(kind: "unknown", message: ""))
        #expect(list[4].readiness == nil)
    }

    // MARK: Tiles

    @Test func tilesShowEveryConnectorOfThisMacWithItsState() {
        let sources = [
            Self.connector("telegram", "Telegram", permissions: [.signIn], readiness: .unsupported(message: "no Telegram app id")),
            Self.connector("apple-mail", "Apple Mail", permissions: [.fullDiskAccess], readiness: Self.noFullDiskAccess),
            Self.connector("imessage", "Messages", permissions: [.fullDiskAccess], connected: true, readiness: .ready),
            Self.connector("whatsapp", "WhatsApp", permissions: [.fullDiskAccess], state: .needsAttention, connected: true),
            Self.connector("gmail", "Gmail", permissions: [.signIn], readiness: Self.noGoogleClient),
            Self.connector("imap", "Mail (IMAP)", permissions: [.signIn], readiness: .ready),
            Self.connector("elsewhere", "Elsewhere", state: .unavailable, connected: true),
            SourceStatus(id: "slack", name: "Slack export", kind: "importer", reads: "An export", state: .importer),
        ]
        let tiles = ConnectSources.tiles(sources)
        #expect(tiles.map(\.id) == ["apple-mail", "imessage", "whatsapp", "gmail", "imap", "telegram"],
                "Importers and connectors this Mac lacks are left out; the ones this build cannot use go last.")
        let states = Dictionary(uniqueKeysWithValues: tiles.map { ($0.id, ConnectSources.state(of: $0)) })
        #expect(states["apple-mail"] == .needsAccess)
        #expect(states["imessage"] == .connected)
        #expect(states["whatsapp"] == .needsAccess)
        #expect(states["gmail"] == .otherWays)
        #expect(states["imap"] == .connect)
        #expect(states["telegram"] == .comingSoon("no Telegram app id"))
        #expect(states["telegram"]?.isEnabled == false)
        #expect(states["telegram"]?.label == "Coming soon")
        #expect(states["imap"]?.label == "Connect")
        #expect(states["imessage"]?.label == "Connected")
        #expect(states["apple-mail"]?.label == "Needs access")
    }

    @Test func anOlderServiceWithoutReadinessStillConnects() {
        let mail = Self.connector("apple-mail", "Apple Mail", permissions: [.fullDiskAccess])
        #expect(ConnectSources.state(of: mail) == .connect)
        let gmail = Self.connector("gmail", "Gmail", permissions: [.signIn])
        #expect(ConnectSources.state(of: gmail) == .connect)
        #expect(ConnectSources.gmailWays(in: [gmail]) == nil)
    }

    @Test func theDefaultSpaceIsChosenFirst() {
        func item(_ name: String, isDefault: Bool = false) -> SpaceItem {
            SpaceItem(url: URL(filePath: "/tmp/\(name).knode"), name: name, isDefault: isDefault, isMissing: false, isExample: false)
        }
        let spaces = [item("Work"), item("Home", isDefault: true), item("Notes")]
        #expect(ConnectSources.initialSpace(spaces, selected: spaces[2].url) == spaces[1].url)
        let noDefault = [item("Work"), item("Notes")]
        #expect(ConnectSources.initialSpace(noDefault, selected: noDefault[1].url) == noDefault[1].url)
        #expect(ConnectSources.initialSpace(noDefault, selected: nil) == noDefault[0].url)
        #expect(ConnectSources.initialSpace([], selected: nil) == nil)
    }

    // MARK: Gmail

    @Test func gmailSignsInWithGoogleWhenTheBuildCan() {
        let sources = [
            Self.connector("gmail", "Gmail", permissions: [.signIn], readiness: .ready),
            Self.connector("apple-mail", "Apple Mail", readiness: .ready),
        ]
        #expect(ConnectSources.gmailWays(in: sources) == nil)
        #expect(ConnectSources.state(of: sources[0]) == .connect)
    }

    @Test func withoutSignInWithGoogleGmailOffersAppleMailThenAnAppPassword() {
        let gmail = Self.connector("gmail", "Gmail", permissions: [.signIn], readiness: Self.noGoogleClient)
        let imap = Self.connector("imap", "Mail (IMAP)", permissions: [.signIn], readiness: .ready)
        func ways(mail: SourceReadiness?, hasMail: Bool = true, hasIMAP: Bool = true) -> ConnectSources.GmailWays? {
            var sources = [gmail]
            if hasMail { sources.append(Self.connector("apple-mail", "Apple Mail", permissions: [.fullDiskAccess], readiness: mail)) }
            if hasIMAP { sources.append(imap) }
            return ConnectSources.gmailWays(in: sources)
        }
        // Mail has an account.
        #expect(ways(mail: .ready) == .init(appleMail: .connect, appPassword: true))
        // Knode cannot tell before it has access: connecting asks for it.
        #expect(ways(mail: Self.noFullDiskAccess) == .init(appleMail: .connect, appPassword: true))
        #expect(ways(mail: nil) == .init(appleMail: .connect, appPassword: true))
        // Mail has no account: add the Gmail account to Mail first.
        #expect(ways(mail: .blocked(kind: "unavailable", message: "add an account to Mail first")) == .init(appleMail: .addAccount, appPassword: true))
        // No Apple Mail connector on this system: only the app password.
        #expect(ways(mail: nil, hasMail: false) == .init(appleMail: nil, appPassword: true))
        #expect(ways(mail: .ready, hasIMAP: false) == .init(appleMail: .connect, appPassword: false))
    }

    @Test func theAppPasswordWayIsFilledInForGmailAndOpensGooglesPage() async throws {
        let client = FakeSourcesClient([Self.connector("imap", "Mail (IMAP)", permissions: [.signIn, .network], readiness: .ready)])
        let opened = SourcesModelTests.Opened()
        let model = SourcesModel(client: client, openURL: { opened.add($0) }, pollInterval: .milliseconds(20))
        await model.select(space)
        let imap = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: imap, preset: .gmailAppPassword))
        await flow.begin()
        guard case .signIn(let step) = flow.step else { Issue.record("no sign-in step: \(flow.step)"); return }
        #expect(step.id == "mail")
        #expect(flow.values["server"] == "imap.gmail.com")
        #expect(flow.preset?.steps.count == 3)
        flow.openPresetLink()
        #expect(opened.all == [URL(string: "https://myaccount.google.com/apppasswords")!])

        flow.values["address"] = "ada@gmail.com"
        flow.values["password"] = "abcd efgh ijkl"
        await flow.submitSignIn()
        #expect(client.actions.contains("sign-in imap start ada@gmail.com imap.gmail.com"))
        #expect(flow.step == .history)
    }

    // MARK: Full Disk Access, noticed by itself

    /// A Knode Service and an app whose access the test turns on.
    final class FakeAccess: Sendable {
        struct State {
            var service: SourceReadiness? = .blocked(kind: "permission", message: "Full Disk Access is needed")
            var app = false
            var checks = 0
            var restarts = 0
            /// Restarting the service gives it the access the app has.
            var restartGrants = true
        }
        let state = Mutex(State())

        var check: SourceAccessCheck {
            SourceAccessCheck(
                service: { _, _ in self.state.withLock { $0.checks += 1; return $0.service } },
                app: { self.state.withLock { $0.app } },
                restartService: {
                    self.state.withLock { state in
                        state.restarts += 1
                        if state.restartGrants, state.app { state.service = .ready }
                    }
                },
                interval: .milliseconds(10)
            )
        }

        func waitForChecks(_ count: Int) async {
            for _ in 0..<500 where state.withLock({ $0.checks }) < count {
                try? await Task.sleep(for: .milliseconds(5))
            }
        }
    }

    func accessFlow(_ access: FakeAccess, opened: SourcesModelTests.Opened) -> SourceConnectFlow {
        let whatsapp = SourcesModelTests.whatsapp()
        return SourceConnectFlow(
            source: whatsapp, space: space, client: FakeSourcesClient([whatsapp]), signIn: nil,
            openURL: { opened.add($0) }, onChange: { _ in }, accessCheck: access.check
        )
    }

    @Test func theAccessStepGoesOnByItselfOnceTheServiceCanRead() async {
        let access = FakeAccess()
        let opened = SourcesModelTests.Opened()
        let flow = accessFlow(access, opened: opened)
        #expect(flow.step == .fullDiskAccess)
        #expect(flow.canWatchAccess)
        let watching = Task { await flow.watchFullDiskAccess() }
        await access.waitForChecks(3)
        #expect(flow.step == .fullDiskAccess)
        #expect(flow.accessWatch == .waiting)
        #expect(opened.all == [SourceConnectFlow.fullDiskAccessURL], "System Settings opens once, at the right pane.")

        access.state.withLock { $0.service = .ready }
        await watching.value
        #expect(flow.step == .history, "Nothing to click: the sheet goes on.")
        #expect(flow.accessWatch == .granted)
        #expect(access.state.withLock { $0.restarts } == 0)
        #expect(opened.all.count == 1)
    }

    @Test func theServiceIsRestartedOnceWhenOnlyTheAppSeesTheAccess() async {
        let access = FakeAccess()
        access.state.withLock { $0.app = true }
        let flow = accessFlow(access, opened: SourcesModelTests.Opened())
        await flow.watchFullDiskAccess()
        #expect(access.state.withLock { $0.restarts } == 1)
        #expect(flow.step == .history)
    }

    @Test func aRestartThatDoesNotHelpIsNotRepeated() async {
        let access = FakeAccess()
        access.state.withLock { $0.app = true; $0.restartGrants = false }
        let flow = accessFlow(access, opened: SourcesModelTests.Opened())
        let watching = Task { await flow.watchFullDiskAccess() }
        await access.waitForChecks(6)
        #expect(access.state.withLock { $0.restarts } == 1)
        #expect(flow.step == .fullDiskAccess)
        watching.cancel()
        await watching.value
    }

    @Test func continuingByHandStopsTheWatch() async {
        let access = FakeAccess()
        let flow = accessFlow(access, opened: SourcesModelTests.Opened())
        let watching = Task { await flow.watchFullDiskAccess() }
        await access.waitForChecks(1)
        await flow.continueFromFullDiskAccess()
        await watching.value
        #expect(flow.step == .history)
    }

    @Test func decisionsOfOneCheck() {
        typealias C = SourceAccessCheck
        #expect(C.decide(service: .ready, appHasAccess: false, restarted: false) == .granted)
        #expect(C.decide(service: .blocked(kind: "unavailable", message: ""), appHasAccess: false, restarted: false) == .granted)
        #expect(C.decide(service: Self.noFullDiskAccess, appHasAccess: false, restarted: false) == .wait)
        #expect(C.decide(service: Self.noFullDiskAccess, appHasAccess: true, restarted: false) == .restartService)
        #expect(C.decide(service: Self.noFullDiskAccess, appHasAccess: true, restarted: true) == .wait)
        #expect(C.decide(service: nil, appHasAccess: true, restarted: false) == .granted)
        #expect(C.decide(service: nil, appHasAccess: false, restarted: false) == .wait)
    }

    @Test func theAppsOwnAccessIsProbedOnFoldersThatExist() {
        let home = URL(filePath: "/Users/ada", directoryHint: .isDirectory)
        func probe(existing: Set<String>, readable: Set<String>) -> Bool {
            FullDiskAccessProbe(
                home: home,
                exists: { existing.contains($0.path(percentEncoded: false)) },
                list: { url in
                    guard readable.contains(url.path(percentEncoded: false)) else { throw CocoaError(.fileReadNoPermission) }
                    return []
                }
            ).hasAccess()
        }
        let safari = "/Users/ada/Library/Safari/"
        let mail = "/Users/ada/Library/Mail/"
        #expect(!probe(existing: [safari, mail], readable: []))
        #expect(probe(existing: [safari, mail], readable: [mail]))
        #expect(!probe(existing: [], readable: [safari]), "A folder that is not there says nothing.")
    }

    // MARK: Icons from web apps

    @Test func webAppsNamedAfterAServiceGiveTheirIcon() {
        let home = URL(filePath: "/Users/ada/Applications", directoryHint: .isDirectory)
        let chrome = home.appending(path: "Chrome Apps.localized", directoryHint: .isDirectory)
        let system = URL(filePath: "/Applications", directoryHint: .isDirectory)
        let folders: [String: [String]] = [
            home.path(percentEncoded: false): ["Gmail.app", "Notes.txt", "Telegram Desktop.app"],
            chrome.path(percentEncoded: false): ["Google Calendar.app", "WA.app"],
            system.path(percentEncoded: false): ["Slack.app", "Gmail.app"],
        ]
        let bundles: [String: WebAppFinder.Bundle] = [
            "/Users/ada/Applications/Gmail.app/": .init(name: "Gmail", bundleID: "com.apple.Safari.WebApp.1234"),
            "/Users/ada/Applications/Telegram Desktop.app/": .init(name: "Telegram Desktop", bundleID: "org.telegram.desktop"),
            "/Users/ada/Applications/Chrome Apps.localized/Google Calendar.app/": .init(name: "Google Calendar", bundleID: "com.google.Chrome.app.abc"),
            "/Users/ada/Applications/Chrome Apps.localized/WA.app/": .init(name: "WhatsApp", bundleID: "com.google.Chrome.app.def"),
            "/Applications/Slack.app/": .init(name: "Slack", bundleID: "com.example.slack"),
            "/Applications/Gmail.app/": .init(name: "Gmail", bundleID: "com.example.gmail"),
        ]
        let finder = WebAppFinder(
            folders: [home, chrome, system],
            contents: { folders[$0.path(percentEncoded: false)] ?? [] },
            bundle: { bundles[$0.path(percentEncoded: false)] }
        )
        #expect(finder.appURL(for: "gmail")?.path(percentEncoded: false) == "/Users/ada/Applications/Gmail.app/")
        #expect(finder.appURL(for: "google-calendar")?.lastPathComponent == "Google Calendar.app")
        #expect(finder.appURL(for: "whatsapp-mac")?.lastPathComponent == "WA.app", "Matched by the name in its bundle.")
        #expect(finder.appURL(for: "slack")?.path(percentEncoded: false) == "/Applications/Slack.app/", "An app named after it counts too.")
        #expect(finder.appURL(for: "telegram") == nil, "Telegram Desktop is not called Telegram.")
        #expect(finder.appURL(for: "imap") == nil)
        #expect(WebAppFinder.isWebApp("com.apple.Safari.WebApp.X"))
        #expect(!WebAppFinder.isWebApp("com.apple.Safari"))
    }

    @Test func aWebAppWinsOverAnAppOfTheSameName() {
        let folder = URL(filePath: "/Applications", directoryHint: .isDirectory)
        let finder = WebAppFinder(
            folders: [folder],
            contents: { _ in ["Gmail.app", "Gmail 2.app"] },
            bundle: { url in
                url.lastPathComponent == "Gmail.app"
                    ? .init(name: "Gmail", bundleID: "com.example.mailer")
                    : .init(name: "Gmail", bundleID: "com.google.Chrome.app.xyz")
            }
        )
        #expect(finder.appURL(for: "gmail")?.lastPathComponent == "Gmail 2.app")
    }

    // MARK: Opening by itself

    @Test func connectYourThingsOpensOnceAfterTheFirstRunSheets() {
        func offer(_ closed: SpaceSheet?, now: SpaceSheet? = nil, offered: Bool = false, hasSpace: Bool = true) -> Bool {
            AppModel.shouldOfferConnectSources(after: closed, now: now, offered: offered, hasSpace: hasSpace)
        }
        #expect(offer(.onboarding))
        #expect(offer(.connectAgents))
        #expect(offer(.newSpace))
        #expect(!offer(.onboarding, offered: true), "Only once.")
        #expect(!offer(.onboarding, hasSpace: false), "Not before there is a space to connect things to.")
        #expect(!offer(.onboarding, now: .connectAgents), "Never over another sheet.")
        #expect(!offer(.connectSources))
        #expect(!offer(nil))
        #expect(!offer(.rename(URL(filePath: "/tmp/A.knode"))))
    }

    @Test func connectYourThingsNeverOpensOverAnotherSheet() {
        let model = AppModel(store: PreviewSpaceStore(), searchDelay: .zero, saveDelay: .seconds(60))
        model.sheet = .onboarding
        model.showConnectSources()
        #expect(model.sheet == .onboarding)
        model.sheet = nil
        model.showConnectSources()
        #expect(model.sheet == .connectSources)
    }

    // MARK: Welcome

    @Test func welcomeButtonsStackWhenTheWindowIsNarrow() throws {
        let model = AppModel(store: PreviewSpaceStore(), searchDelay: .zero, saveDelay: .seconds(60))
        func size(width: CGFloat) throws -> CGSize {
            let renderer = ImageRenderer(content: WelcomeActions().environment(model).knodeTheme().frame(width: width))
            return try #require(renderer.nsImage).size
        }
        let wide = try size(width: 1000)
        let narrow = try size(width: 360)
        #expect(narrow.height > wide.height * 2, "One under the other, each label whole, rather than cut short in a row.")
    }
}
