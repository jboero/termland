import SwiftUI

@main
struct TermlandApp: App {
    @StateObject private var model = HomeModel()

    var body: some Scene {
        WindowGroup {
            HomeView(model: model)
        }

        #if os(macOS)
        // One window per streaming session, opened by HomeView's openWindow.
        WindowGroup("Session", for: SessionLaunch.self) { $launch in
            if let launch {
                SessionWindow(home: model, launch: launch)
            }
        }
        .defaultSize(width: 1280, height: 800)
        #endif
    }
}

#if os(macOS)
private struct SessionWindow: View {
    @ObservedObject var home: HomeModel
    let launch: SessionLaunch
    @Environment(\.dismissWindow) private var dismissWindow

    var body: some View {
        SessionContainer(home: home, launch: launch) { dismissWindow() }
            .frame(minWidth: 640, minHeight: 400)
    }
}
#endif
