import Foundation
import Synchronization
import Testing
@testable import Knode

/// A stand-in for Contacts: invented people, and whether access was given. It never asks the
/// system for access and never reads the real address book.
final class FakeContactStore: ContactStore {
    struct State {
        var access: ContactAccess
        /// What the person answers when asked for access.
        var grants: Bool
        var requests = 0
        var reads = 0
        var entries: [ContactEntry]
    }

    let state: Mutex<State>

    init(access: ContactAccess = .notDetermined, grants: Bool = true, entries: [ContactEntry] = FakeContactStore.people) {
        state = Mutex(State(access: access, grants: grants, entries: entries))
    }

    static let people = [
        ContactEntry(name: "Ada Lovelace", handles: ["+1 (555) 010-0123", "Ada@Example.com"]),
        ContactEntry(name: "Grace Hopper", handles: ["020 7946 0958"]),
        ContactEntry(name: "  ", handles: ["+15550100999"]),
    ]

    var access: ContactAccess { state.withLock { $0.access } }
    var requests: Int { state.withLock { $0.requests } }
    var reads: Int { state.withLock { $0.reads } }

    func setAccess(_ access: ContactAccess) { state.withLock { $0.access = access } }
    func setEntries(_ entries: [ContactEntry]) { state.withLock { $0.entries = entries } }

    func requestAccess() async throws -> Bool {
        state.withLock { state in
            state.requests += 1
            state.access = state.grants ? .granted : .denied
            return state.grants
        }
    }

    func entries() async throws -> [ContactEntry] {
        state.withLock { state in
            state.reads += 1
            return state.access == .granted ? state.entries : []
        }
    }
}

@MainActor
@Suite(.serialized) struct ContactNamesTests {
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    static let messages = SourceStatus(
        id: "imessage", name: "Messages", kind: "connector", reads: "The iMessage and SMS conversations of Messages on this Mac",
        permissions: [.fullDiskAccess], container: "chat", state: .off
    )

    /// Preferences of their own, emptied for each test.
    func preferences() -> UserDefaults {
        let name = "md.knode.tests.contact-names"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    @Test func handlesAreNormalisedAsTheServiceDoes() {
        // The same cases as `handles_are_normalised_as_the_app_does` in knode-service.
        let cases = [
            ("+1 (555) 010-0123", "+15550100123"),
            ("0044 20 7946 0958", "+442079460958"),
            ("tel:+34 600 000 000", "+34600000000"),
            ("020 7946 0958", "2079460958"),
            ("555.010.0123", "5550100123"),
            (" Ada@Example.COM ", "ada@example.com"),
            ("mailto:grace@example.com", "grace@example.com"),
            ("Apple", "apple"),
            ("12345", "12345"),
        ]
        for (raw, normal) in cases {
            #expect(ContactHandle.normalize(raw) == normal, "\(raw)")
        }
        #expect(ContactHandle.normalize("  ") == nil)
    }

    @Test func turningNamesOnAsksForAccessThenGivesTheServiceTheNames() async {
        let client = FakeSourcesClient([Self.messages])
        let contacts = FakeContactStore()
        let defaults = preferences()
        let model = SourcesModel(client: client, contacts: contacts, preferences: defaults)
        await model.select(space)
        #expect(contacts.requests == 0, "Nothing is asked until the person turns names on.")
        #expect(contacts.reads == 0)

        await model.setNamesFromContacts(true)
        #expect(contacts.requests == 1)
        #expect(model.contactNames.isOn)
        #expect(defaults.bool(forKey: ContactNames.preferenceKey))
        #expect(client.contactNames == [
            "+15550100123": "Ada Lovelace",
            "ada@example.com": "Ada Lovelace",
            "2079460958": "Grace Hopper",
        ])

        await model.setNamesFromContacts(false)
        #expect(!model.contactNames.isOn)
        #expect(client.contactNames == [:])
        #expect(!defaults.bool(forKey: ContactNames.preferenceKey))
    }

    @Test func refusedAccessSendsNothingAndSaysWhere() async {
        let client = FakeSourcesClient([Self.messages])
        let contacts = FakeContactStore(grants: false)
        let model = SourcesModel(client: client, contacts: contacts, preferences: preferences())
        await model.select(space)
        await model.setNamesFromContacts(true)

        #expect(!model.contactNames.isOn)
        #expect(client.contactNames == nil)
        #expect(model.errorMessage?.contains("Privacy & Security > Contacts") == true)

        // Asked once: the next time macOS answers by itself, and the app says the same.
        await model.setNamesFromContacts(true)
        #expect(contacts.requests == 1)
        #expect(client.contactNames == nil)
    }

    @Test func namesAreGivenAgainWhenSourcesOpenAndForgottenWhenAccessIsTakenAway() async {
        let client = FakeSourcesClient([Self.messages])
        let contacts = FakeContactStore(access: .granted)
        let defaults = preferences()
        defaults.set(true, forKey: ContactNames.preferenceKey)
        contacts.setEntries([ContactEntry(name: "Ada King", handles: ["+15550100123"])])

        let model = SourcesModel(client: client, contacts: contacts, preferences: defaults)
        await model.select(space)
        #expect(client.contactNames == ["+15550100123": "Ada King"])
        #expect(contacts.requests == 0)

        contacts.setAccess(.denied)
        let again = SourcesModel(client: client, contacts: contacts, preferences: defaults)
        await again.select(space)
        #expect(client.contactNames == [:])
        #expect(!again.contactNames.isOn)
        #expect(!defaults.bool(forKey: ContactNames.preferenceKey))
    }

    @Test func theConnectSheetOffersNamesOnlyForMessagesBeforeConnecting() async throws {
        let client = FakeSourcesClient([Self.messages, SourcesModelTests.whatsapp()])
        let model = SourcesModel(client: client, contacts: FakeContactStore(), preferences: preferences())
        await model.select(space)

        let whatsappSource = try #require(model.connectors.first { $0.id == "whatsapp" })
        let whatsapp = try #require(model.connectFlow(for: whatsappSource))
        #expect(!whatsapp.offersContactNames)

        let messages = try #require(model.connectors.first { $0.id == "imessage" })
        let flow = try #require(model.connectFlow(for: messages))
        #expect(flow.offersContactNames)
        await flow.continueFromFullDiskAccess()
        #expect(flow.step == .history)
        await flow.setNamesFromContacts(true)
        await flow.connect()
        await flow.finish()

        #expect(model.contactNames.isOn)
        #expect(client.actions.prefix(2) == ["contact-names 3", "connect imessage from-today paused"],
                "Names reach the service before the connector reads anything.")
        #expect(client.actions.last == "resume imessage")
    }
}
