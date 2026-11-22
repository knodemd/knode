import Foundation
import Testing
@testable import Knode

/// The largest file a connector downloads (Telegram): asked for in the Connect sheet and set
/// before the connector reads anything, then changed from Settings > Sources.
@MainActor
@Suite(.serialized) struct SourcesMediaLimitTests {
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    static func telegram(connected: Bool = false, maxMB: Int64? = nil) -> SourceStatus {
        SourceStatus(
            id: "telegram", name: "Telegram", kind: "connector", reads: "Your Telegram chats", permissions: [.network],
            container: "chat", media: true, state: connected ? .idle : .off,
            setting: connected ? SourceStatus.Setting(on: true, history: .fromToday, connectedAt: 1, since: 0, mediaMaxMB: maxMB) : nil
        )
    }

    func model(_ client: FakeSourcesClient) -> SourcesModel {
        SourcesModel(client: client, pollInterval: .milliseconds(20))
    }

    @Test func decodesMediaAndTheLimitFromTheService() throws {
        let json = """
        {"id": "telegram", "name": "Telegram", "kind": "connector", "reads": "Chats", "media": true, "state": "idle",
         "setting": {"on": true, "history": "from-today", "connected_at": 1, "since": 0, "media_max_mb": 5}}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let status = try decoder.decode(SourceStatus.self, from: Data(json.utf8))
        #expect(status.media)
        #expect(status.setting?.mediaMaxMB == 5)

        let plain = try decoder.decode(SourceStatus.self, from: Data(#"{"id": "whatsapp", "state": "off"}"#.utf8))
        #expect(!plain.media)
    }

    @Test func parsesWhatThePersonTyped() {
        #expect(MediaLimit("")?.megabytes == .some(nil))
        #expect(MediaLimit(" 50 ")?.megabytes == 50)
        #expect(MediaLimit("0")?.megabytes == 0)
        #expect(MediaLimit("-1") == nil)
        #expect(MediaLimit("2.5") == nil)
        #expect(MediaLimit("lots") == nil)
    }

    @Test func theConnectSheetSetsTheLimitBetweenConnectingPausedAndResuming() async throws {
        let client = FakeSourcesClient([Self.telegram()])
        let model = model(client)
        await model.select(space)
        let source = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: source))
        #expect(flow.step == .history)
        #expect(flow.offersMediaLimit)
        #expect(flow.mediaLimit == "", "Empty: the connector's default.")

        flow.mediaLimit = "abc"
        #expect(!flow.isMediaLimitValid)
        await flow.connect()
        #expect(flow.step == .history)
        #expect(flow.errorMessage == MediaLimit.invalidMessage)
        #expect(client.actions.isEmpty, "Nothing is connected with a limit that is not a size.")

        flow.mediaLimit = "50"
        await flow.connect()
        await flow.finish()
        #expect(client.actions == ["connect telegram from-today paused", "media telegram 50", "containers telegram", "resume telegram"])
        #expect(client.source("telegram")?.setting?.mediaMaxMB == 50)
    }

    @Test func anEmptyFieldKeepsTheConnectorsDefault() async throws {
        let client = FakeSourcesClient([Self.telegram()])
        let model = model(client)
        await model.select(space)
        let source = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: source))
        await flow.connect()
        #expect(client.actions.prefix(2) == ["connect telegram from-today paused", "media telegram default"])
        #expect(client.source("telegram")?.setting?.mediaMaxMB == nil)
    }

    @Test func sourcesWithoutMediaAreNotAsked() async throws {
        let client = FakeSourcesClient([SourcesModelTests.whatsapp()])
        let model = model(client)
        await model.select(space)
        let source = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: source))
        #expect(!flow.offersMediaLimit)
        await flow.continueFromFullDiskAccess()
        await flow.connect()
        #expect(!client.actions.contains { $0.hasPrefix("media") })
    }

    @Test func settingsChangeTheLimitOfAConnectedSource() async throws {
        let client = FakeSourcesClient([Self.telegram(connected: true, maxMB: 20)])
        let model = model(client)
        await model.select(space)
        let connected = try #require(model.connectors.first)
        #expect(MediaLimit.text(connected.setting?.mediaMaxMB) == "20")

        await model.setMediaLimit("20", for: connected)
        #expect(client.actions.isEmpty, "The same limit: nothing is sent.")

        await model.setMediaLimit("0", for: connected)
        #expect(client.actions == ["media telegram 0"])
        #expect(model.connectors.first?.setting?.mediaMaxMB == 0)

        let zero = try #require(model.connectors.first)
        await model.setMediaLimit("much", for: zero)
        #expect(model.errorMessage == MediaLimit.invalidMessage)
        #expect(client.actions == ["media telegram 0"])

        await model.setMediaLimit("", for: zero)
        #expect(client.actions == ["media telegram 0", "media telegram default"])
        #expect(model.errorMessage == nil)
    }
}
