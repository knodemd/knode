import Contacts
import Foundation
import Observation

/// Whether the person let Knode read Contacts.
public enum ContactAccess: Sendable, Equatable {
    case notDetermined
    case granted
    case denied
}

/// One contact: its name, and the phone numbers and email addresses that reach it.
public struct ContactEntry: Sendable, Hashable {
    public var name: String
    public var handles: [String]

    public init(name: String, handles: [String]) {
        self.name = name
        self.handles = handles
    }
}

/// Where the app reads the names of people: Contacts on this Mac, or a fake in tests.
public protocol ContactStore: Sendable {
    var access: ContactAccess { get }
    /// Asks the person for access, the first time; whether it was given.
    func requestAccess() async throws -> Bool
    /// Every contact with a name and a phone number or an email address.
    func entries() async throws -> [ContactEntry]
}

/// Contacts on this Mac, read only after the person turned names on.
public struct SystemContactStore: ContactStore {
    public init() {}

    public var access: ContactAccess {
        switch CNContactStore.authorizationStatus(for: .contacts) {
        case .notDetermined: .notDetermined
        case .denied, .restricted: .denied
        default: .granted
        }
    }

    public func requestAccess() async throws -> Bool {
        try await CNContactStore().requestAccess(for: .contacts)
    }

    public func entries() async throws -> [ContactEntry] {
        try await Task.detached(priority: .utility) {
            let keys: [any CNKeyDescriptor] = [
                CNContactFormatter.descriptorForRequiredKeys(for: .fullName),
                CNContactOrganizationNameKey as CNKeyDescriptor,
                CNContactPhoneNumbersKey as CNKeyDescriptor,
                CNContactEmailAddressesKey as CNKeyDescriptor,
            ]
            let request = CNContactFetchRequest(keysToFetch: keys)
            var entries: [ContactEntry] = []
            try CNContactStore().enumerateContacts(with: request) { contact, _ in
                let full = CNContactFormatter.string(from: contact, style: .fullName) ?? ""
                let name = full.isEmpty ? contact.organizationName : full
                let handles = contact.phoneNumbers.map(\.value.stringValue) + contact.emailAddresses.map { $0.value as String }
                if !name.isEmpty, !handles.isEmpty { entries.append(ContactEntry(name: name, handles: handles)) }
            }
            return entries
        }.value
    }
}

/// A store with no contacts that never asks for access: for previews and when nothing is wired.
public struct NoContactStore: ContactStore {
    public init() {}
    public var access: ContactAccess { .denied }
    public func requestAccess() async throws -> Bool { false }
    public func entries() async throws -> [ContactEntry] { [] }
}

/// Phone numbers and email addresses as Knode Service compares them (`normalize_handle` in
/// `knode-service`): an email address in lower case; a phone number in E.164 when it has a
/// country code (`+` or `00`), else its digits without a trunk prefix's leading zeros.
enum ContactHandle {
    static func normalize(_ handle: String) -> String? {
        var h = handle.trimmingCharacters(in: .whitespacesAndNewlines)
        for prefix in ["tel:", "mailto:", "sms:"] where h.lowercased().hasPrefix(prefix) {
            h = String(h.dropFirst(prefix.count)).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        guard !h.isEmpty else { return nil }
        if h.contains("@") { return h.lowercased() }
        let allowed = Set("0123456789+-.()/ \u{a0}")
        let digits = String(h.filter(\.isASCIIDigit))
        guard h.allSatisfy(allowed.contains), !digits.isEmpty else { return h.lowercased() }
        if h.hasPrefix("+") { return "+" + digits }
        if digits.hasPrefix("00") { return "+" + digits.dropFirst(2) }
        let national = String(digits.drop { $0 == "0" })
        return national.isEmpty ? digits : national
    }

    /// Every handle of the contacts, normalised, with its name: the first contact wins.
    static func map(_ entries: [ContactEntry]) -> [String: String] {
        var map: [String: String] = [:]
        for entry in entries {
            let name = entry.name.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !name.isEmpty else { continue }
            for handle in entry.handles {
                if let key = normalize(handle), map[key] == nil { map[key] = name }
            }
        }
        return map
    }
}

private extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}

/// Names from Contacts for the connectors that show people by phone number or email address
/// (Messages). Knode Service cannot read Contacts, so the app asks for access when the person
/// turns names on, reads them, and gives the service a map of handles to names
/// (`sources.contact_names`). It gives them again whenever Settings > Sources opens, so changes
/// in Contacts reach it; turning names off gives an empty map, which forgets them.
@MainActor
@Observable
final class ContactNames {
    /// Where the person's choice is remembered.
    static let preferenceKey = "sources.namesFromContacts"
    /// The connectors that name people from Contacts.
    static let sourceIDs: Set<String> = ["imessage"]

    /// Whether names from Contacts are on.
    private(set) var isOn: Bool
    private(set) var isBusy = false

    @ObservationIgnored private let store: any ContactStore
    @ObservationIgnored private let client: any SourcesClient
    @ObservationIgnored private let preferences: UserDefaults?

    /// - Parameter preferences: where the choice is remembered; `nil` remembers nothing.
    init(store: any ContactStore, client: any SourcesClient, preferences: UserDefaults?) {
        self.store = store
        self.client = client
        self.preferences = preferences
        isOn = preferences?.bool(forKey: Self.preferenceKey) ?? false
    }

    func supports(_ source: SourceStatus) -> Bool { Self.sourceIDs.contains(source.id) }

    /// Turns names on (asking for access the first time) or off. Throws when access is refused
    /// or the service could not take them; the choice then stays as it was.
    func setOn(_ on: Bool) async throws {
        guard on != isOn, !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        if on {
            try await send(try await names())
        } else {
            try await client.setContactNames([:])
        }
        remember(on)
    }

    /// Gives the service the names again, when they are on: Contacts may have changed, or the
    /// service may have lost them. When access was taken away meanwhile, turns names off.
    func refresh() async {
        guard isOn, !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        guard store.access == .granted else {
            if (try? await client.setContactNames([:])) != nil { remember(false) }
            return
        }
        if let entries = try? await store.entries() { try? await send(ContactHandle.map(entries)) }
    }

    private func names() async throws -> [String: String] {
        switch store.access {
        case .granted: break
        case .denied: throw ContactNamesError.denied
        case .notDetermined:
            guard try await store.requestAccess() else { throw ContactNamesError.denied }
        }
        return ContactHandle.map(try await store.entries())
    }

    private func send(_ names: [String: String]) async throws {
        _ = try await client.setContactNames(names)
    }

    private func remember(_ on: Bool) {
        isOn = on
        preferences?.set(on, forKey: Self.preferenceKey)
    }
}

enum ContactNamesError: LocalizedError {
    case denied

    var errorDescription: String? {
        "Knode can't read Contacts. Allow Knode in System Settings > Privacy & Security > Contacts, then turn names on again."
    }
}
