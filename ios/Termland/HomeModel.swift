import Foundation
import Combine

@MainActor
final class HomeModel: ObservableObject {
    enum SessionsState: Equatable {
        case idle
        case loading
        case loaded([SessionSummary])
        case failed(String)
    }

    @Published private(set) var profiles: [HostProfile] = []
    @Published var selectedProfileID: UUID?
    @Published private(set) var sessions: SessionsState = .idle

    private let profilesKey = "dev.termland.ios.profiles.v1"
    private let client = TermlandClient()

    // Deliberately narrow test seam: the simulator smoke test proves the
    // generated UniFFI binding and embedded XCFramework can be loaded.
    var clientIsConnectedForTesting: Bool { client.isConnected() }

    init() {
        profiles = Self.loadProfiles(key: profilesKey)
        selectedProfileID = profiles.first?.id
    }

    var selectedProfile: HostProfile? {
        profiles.first { $0.id == selectedProfileID }
    }

    func save(_ profile: HostProfile, password: String) throws {
        guard !profile.host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw ProfileError.invalidHost
        }
        guard (1...65_535).contains(profile.port) else { throw ProfileError.invalidPort }
        try KeychainStore.save(password, for: profile.id)
        if let index = profiles.firstIndex(where: { $0.id == profile.id }) {
            profiles[index] = profile
        } else {
            profiles.append(profile)
        }
        persistProfiles()
        selectedProfileID = profile.id
    }

    func delete(_ profile: HostProfile) throws {
        try KeychainStore.delete(for: profile.id)
        profiles.removeAll { $0.id == profile.id }
        if selectedProfileID == profile.id { selectedProfileID = profiles.first?.id }
        sessions = .idle
        persistProfiles()
    }

    func password(for profile: HostProfile) -> String {
        (try? KeychainStore.password(for: profile.id)) ?? ""
    }

    func refreshSessions(for profile: HostProfile) {
        sessions = .loading
        let coreProfile = profile.coreProfile(password: password(for: profile))
        let client = client
        Task.detached {
            do {
                let result = try client.listSessions(profile: coreProfile)
                await self.publish(.loaded(result), for: profile.id)
            } catch {
                await self.publish(.failed(TermlandErrorText.describe(error, profile: profile)), for: profile.id)
            }
        }
    }

    func close(_ session: SessionSummary, on profile: HostProfile) {
        sessions = .loading
        let coreProfile = profile.coreProfile(password: password(for: profile))
        let client = client
        Task.detached {
            do {
                try client.closeSession(profile: coreProfile, sessionId: session.sessionId)
                let result = try client.listSessions(profile: coreProfile)
                await self.publish(.loaded(result), for: profile.id)
            } catch {
                await self.publish(.failed(TermlandErrorText.describe(error, profile: profile)), for: profile.id)
            }
        }
    }

    /// Requests are not cancelled when the selection changes, and a slow or
    /// unreachable server can answer after a faster one. Drop results for a
    /// profile that is no longer selected, or server A's sessions would be
    /// shown (and closable) under server B.
    private func publish(_ state: SessionsState, for profileID: UUID) {
        guard selectedProfileID == profileID else { return }
        sessions = state
    }

    private func persistProfiles() {
        guard let data = try? JSONEncoder().encode(profiles) else { return }
        UserDefaults.standard.set(data, forKey: profilesKey)
    }

    private static func loadProfiles(key: String) -> [HostProfile] {
        guard let data = UserDefaults.standard.data(forKey: key),
              let saved = try? JSONDecoder().decode([HostProfile].self, from: data) else { return [] }
        return saved
    }
}

enum ProfileError: LocalizedError {
    case invalidHost
    case invalidPort

    var errorDescription: String? {
        switch self {
        case .invalidHost: return "Enter a server host name or IP address."
        case .invalidPort: return "Port must be between 1 and 65535."
        }
    }
}
