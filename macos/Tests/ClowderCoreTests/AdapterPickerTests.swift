import XCTest
@testable import ClowderCore

@MainActor
final class AdapterPickerTests: XCTestCase {
    func testListAdaptersEncodes() throws {
        let data = try JSONEncoder().encode(ControlRequest.listAdapters)
        let o = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(o["type"] as? String, "listAdapters")
    }

    func testAdapterListDecodes() throws {
        let json = #"{"type":"adapterList","adapters":[{"id":"codex","displayName":"OpenAI Codex"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .adapterList([AdapterInfo(id: "codex", displayName: "OpenAI Codex")]))
    }

    func testStoreApplyAdapterListSetsAdapters() {
        let store = AgentStore()
        store.apply(.adapterList([AdapterInfo(id: "codex", displayName: "OpenAI Codex")]))
        XCTAssertEqual(store.adapters, [AdapterInfo(id: "codex", displayName: "OpenAI Codex")])
    }

    func testConnectRequestsAdapters() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listAdapters\"") },
                      "connect must request the adapter list")
    }

    func testAppModelForwardsStoreAdapters() {
        let store = AgentStore()
        let model = AppModel(store: store, makeTransport: { FakeControlTransport() })
        store.apply(.adapterList([AdapterInfo(id: "shell", displayName: "Shell")]))
        XCTAssertEqual(model.adapters, [AdapterInfo(id: "shell", displayName: "Shell")])
    }
}
