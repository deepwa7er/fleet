import Foundation
import Observation

@MainActor
@Observable
final class ClipStore {
    private(set) var clip: Clip?
    private(set) var isLoading: Bool = false
    var error: String?
    private(set) var serverURL: URL

    private let cache: ClipCache?
    private let cacheError: String?

    init() {
        self.serverURL = Settings.serverURL()

        let openedCache: ClipCache?
        let openError: String?
        var initialClip: Clip?

        do {
            let cache = try ClipCache()
            openedCache = cache
            do {
                initialClip = try cache.read().current
                openError = nil
            } catch {
                openError = error.localizedDescription
            }
        } catch {
            openedCache = nil
            openError = error.localizedDescription
        }

        self.cache = openedCache
        self.cacheError = openError
        self.error = openError
        self.clip = initialClip
    }

    func refresh() async {
        isLoading = true
        defer { isLoading = false }

        let client = TidepoolClient(baseURL: serverURL)
        do {
            let fresh = try await client.currentClip()
            self.clip = fresh
            self.error = cacheError
            do {
                try cache?.update(current: fresh)
            } catch {
                self.error = error.localizedDescription
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func updateServerURL(_ url: URL) {
        do {
            try Settings.setServerURL(url)
            self.serverURL = url
        } catch {
            self.error = error.localizedDescription
        }
    }
}
