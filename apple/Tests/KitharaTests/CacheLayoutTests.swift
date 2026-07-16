import KitharaFFI
import Testing
@testable import Kithara

@Suite("CacheLayoutRegistry")
struct CacheLayoutTests {
    @Test("file and HLS registrations remain independent")
    func registrationsRemainIndependent() {
        let file = TestCacheLayout(id: "file")
        let hls = TestCacheLayout(id: "hls")
        var registry = CacheLayoutRegistry()

        registry.register(hls, for: .hls)
        registry.register(file, for: .file)

        let registrations = registry.ffiRegistrations
        #expect(registrations.map(\.target) == [.file, .hls])
        #expect(registrations.first?.layout === file)
        #expect(registrations.last?.layout === hls)
    }

    @Test("last registration replaces an earlier layout")
    func lastRegistrationWins() {
        let first = TestCacheLayout(id: "first")
        let replacement = TestCacheLayout(id: "replacement")
        var registry = CacheLayoutRegistry()

        registry.register(first, for: .file)
        registry.register(replacement, for: .file)

        let registrations = registry.ffiRegistrations
        #expect(registrations.count == 1)
        #expect(registrations.first?.target == .file)
        #expect(registrations.first?.layout === replacement)
    }
}

private final class TestCacheLayout: CacheLayoutDelegate, @unchecked Sendable {
    let id: String

    init(id: String) {
        self.id = id
    }

    func root(source: CacheAssetSource) -> String {
        id
    }

    func path(resource: CacheAssetResource) -> String {
        id
    }
}
