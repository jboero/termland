import SwiftUI

@main
struct TermlandApp: App {
    @StateObject private var model = HomeModel()

    var body: some Scene {
        WindowGroup {
            HomeView(model: model)
        }
    }
}
