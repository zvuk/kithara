import Testing
@testable import Kithara

@Suite("Asset store")
struct AssetStoreTests {
    @Test("the native registry retains registered layouts")
    func registryRetainsLayout() {
        let registry = AssetLayoutRegistry()
        weak var retained: TestAssetLayout?

        do {
            let layout = TestAssetLayout(id: "file")
            retained = layout
            registry.register(layout, for: .file)
        }

        #expect(retained != nil)
    }

    @Test("the last native registration replaces the previous layout")
    func lastRegistrationWins() {
        let registry = AssetLayoutRegistry()
        weak var replaced: TestAssetLayout?

        do {
            let layout = TestAssetLayout(id: "first")
            replaced = layout
            registry.register(layout, for: .file)
        }
        registry.register(TestAssetLayout(id: "replacement"), for: .file)

        #expect(replaced == nil)
    }

    @Test("an asset store owns an immutable registry snapshot")
    func storeOwnsRegistrySnapshot() {
        let registry = AssetLayoutRegistry()
        weak var captured: TestAssetLayout?
        var store: AssetStore?

        do {
            let layout = TestAssetLayout(id: "captured")
            captured = layout
            registry.register(layout, for: .file)
            store = AssetStore(layouts: registry)
        }
        registry.register(TestAssetLayout(id: "replacement"), for: .file)

        #expect(store != nil)
        #expect(captured != nil)
        store = nil
        #expect(captured == nil)
    }
}

private final class TestAssetLayout: AssetLayout, @unchecked Sendable {
    let id: String

    init(id: String) {
        self.id = id
    }

    func root(source: AssetSource) -> String {
        id
    }

    func path(resource: AssetResource) -> String {
        id
    }
}
