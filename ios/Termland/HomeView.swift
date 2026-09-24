import SwiftUI

struct HomeView: View {
    @ObservedObject var model: HomeModel
    @State private var editing: HostProfile?
    @State private var profileToDelete: HostProfile?

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selectedProfileID) {
                ForEach(model.profiles) { profile in
                    VStack(alignment: .leading) {
                        Text(profile.displayName.isEmpty ? "New server" : profile.displayName)
                        Text(profile.subtitle).font(.caption).foregroundStyle(.secondary)
                    }
                    .tag(profile.id)
                    .contextMenu {
                        Button("Edit") { editing = profile }
                        Button("Delete", role: .destructive) { profileToDelete = profile }
                    }
                }
            }
            .navigationTitle("Termland")
            .toolbar {
                Button { editing = HostProfile() } label: { Label("Add Server", systemImage: "plus") }
            }
        } detail: {
            if let profile = model.selectedProfile {
                SessionListView(model: model, profile: profile, editing: $editing, deleting: $profileToDelete)
            } else {
                ContentUnavailableView("Add a server", systemImage: "desktopcomputer", description: Text("Save a Termland server profile to list its resumable sessions."))
            }
        }
        .sheet(item: $editing) { profile in
            ProfileEditor(profile: profile, password: model.password(for: profile)) { saved, password in
                try model.save(saved, password: password)
            }
        }
        .alert("Delete server profile?", isPresented: Binding(
            get: { profileToDelete != nil }, set: { if !$0 { profileToDelete = nil } }
        ), presenting: profileToDelete) { profile in
            Button("Delete", role: .destructive) { try? model.delete(profile) }
            Button("Cancel", role: .cancel) {}
        } message: { profile in
            Text("This removes \(profile.displayName) and its saved password from this device. Remote sessions are not affected.")
        }
    }
}

private struct SessionListView: View {
    @ObservedObject var model: HomeModel
    let profile: HostProfile
    @Binding var editing: HostProfile?
    @Binding var deleting: HostProfile?
    @State private var closeCandidate: SessionSummary?

    var body: some View {
        List {
            Section {
                Text(profile.subtitle).foregroundStyle(.secondary)
                if profile.useTLS && profile.acceptInvalidCertificates {
                    Label("Certificate verification is disabled for this profile.", systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                }
            }
            Section("Resumable sessions") {
                switch model.sessions {
                case .idle:
                    ContentUnavailableView("No session list loaded", systemImage: "arrow.clockwise", description: Text("Tap Refresh to query this server."))
                case .loading:
                    HStack { Spacer(); ProgressView(); Spacer() }
                case .failed(let message):
                    ContentUnavailableView("Couldn’t load sessions", systemImage: "exclamationmark.triangle", description: Text(message))
                case .loaded(let sessions):
                    if sessions.isEmpty { Text("No resumable sessions.").foregroundStyle(.secondary) }
                    ForEach(sessions, id: \.sessionId) { session in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(session.mode.capitalized)
                            Text("\(session.width) × \(session.height) · \(ageText(session.ageSecs))\(session.attached ? " · attached" : "")")
                                .font(.caption).foregroundStyle(.secondary)
                            Text(session.sessionId).font(.caption2).foregroundStyle(.tertiary)
                        }
                        .swipeActions {
                            Button("Close", role: .destructive) { closeCandidate = session }
                        }
                    }
                }
            }
            Section {
                Text("Streaming and resume open in the next VideoToolbox milestone. This screen only manages persistent sessions.")
                    .font(.footnote).foregroundStyle(.secondary)
            }
        }
        .navigationTitle(profile.displayName)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button { model.refreshSessions(for: profile) } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                Button { editing = profile } label: { Label("Edit", systemImage: "slider.horizontal.3") }
            }
        }
        .task(id: profile.id) { model.refreshSessions(for: profile) }
        .confirmationDialog("Close remote session?", isPresented: Binding(
            get: { closeCandidate != nil }, set: { if !$0 { closeCandidate = nil } }
        ), presenting: closeCandidate) { session in
            Button("Close Session", role: .destructive) { model.close(session, on: profile) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in Text("This permanently ends the remote session. It cannot be resumed.") }
    }

    private func ageText(_ seconds: UInt64) -> String {
        if seconds < 60 { return "\(seconds)s old" }
        if seconds < 3_600 { return "\(seconds / 60)m old" }
        return "\(seconds / 3_600)h old"
    }
}

private struct ProfileEditor: View {
    @Environment(\.dismiss) private var dismiss
    @State private var profile: HostProfile
    @State private var password: String
    @State private var error: String?
    let save: (HostProfile, String) throws -> Void

    init(profile: HostProfile, password: String, save: @escaping (HostProfile, String) throws -> Void) {
        _profile = State(initialValue: profile)
        _password = State(initialValue: password)
        self.save = save
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Server") {
                    TextField("Label", text: $profile.label)
                    hostField
                    portField
                }
                Section("Connection") {
                    Toggle("Use TLS", isOn: $profile.useTLS)
                    if profile.useTLS {
                        Toggle("Accept invalid certificate", isOn: $profile.acceptInvalidCertificates)
                        if profile.acceptInvalidCertificates {
                            Text("Use only for a server you trust. This disables server identity verification and permits man-in-the-middle attacks.")
                                .font(.footnote).foregroundStyle(.orange)
                        }
                    }
                }
                Section("Credentials") {
                    usernameField
                    SecureField("Password", text: $password)
                    Text("The password is stored in this device’s Keychain, never in the profile file.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                if let error { Text(error).foregroundStyle(.red) }
            }
            .navigationTitle(profile.label.isEmpty ? "Server" : "Edit Server")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        do { try save(profile, password); dismiss() }
                        catch { self.error = error.localizedDescription }
                    }
                }
            }
        }
    }

    private var hostField: some View {
        #if os(iOS)
        TextField("Host", text: $profile.host)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
        #else
        TextField("Host", text: $profile.host)
        #endif
    }

    private var portField: some View {
        #if os(iOS)
        TextField("Port", value: $profile.port, format: .number).keyboardType(.numberPad)
        #else
        TextField("Port", value: $profile.port, format: .number)
        #endif
    }

    private var usernameField: some View {
        #if os(iOS)
        TextField("Username (optional)", text: $profile.username)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
        #else
        TextField("Username (optional)", text: $profile.username)
        #endif
    }
}
