import Foundation

/// UI-owned profile data. Credentials deliberately do not live in this
/// Codable record: only KeychainStore stores the password, indexed by `id`.
struct HostProfile: Identifiable, Codable, Equatable {
    var id = UUID()
    var label = ""
    var host = ""
    var port = 7867
    var useTLS = true
    var acceptInvalidCertificates = false
    var username = ""

    var displayName: String {
        label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? host : label
    }

    var subtitle: String {
        let identity = username.trimmingCharacters(in: .whitespacesAndNewlines)
        let endpoint = "\(host):\(port)"
        let transport = useTLS ? "TLS" : "Plain TCP"
        let verification = useTLS && acceptInvalidCertificates ? " · unverified" : ""
        return identity.isEmpty ? "\(endpoint) · \(transport)\(verification)" : "\(identity)@\(endpoint) · \(transport)\(verification)"
    }

    func coreProfile(password: String?) -> ServerProfile {
        ServerProfile(
            host: host.trimmingCharacters(in: .whitespacesAndNewlines),
            port: UInt16(clamping: port),
            useTls: useTLS,
            acceptInvalidCerts: acceptInvalidCertificates,
            username: username.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            password: password?.nilIfEmpty,
            useSsh: false,
            useQuic: false
        )
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
