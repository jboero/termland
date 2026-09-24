import XCTest
@testable import Termland

final class TermlandTests: XCTestCase {
    @MainActor
    func testClientStartsDisconnected() {
        let connected = HomeModel().clientIsConnectedForTesting
        XCTAssertFalse(connected)
    }
}
