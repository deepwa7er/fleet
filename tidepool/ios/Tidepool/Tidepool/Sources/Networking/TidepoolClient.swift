import Foundation

struct TidepoolClient {
    enum ClientError: Error, LocalizedError {
        case invalidResponse(status: Int)
        case decode(underlying: Error)
        case transport(underlying: Error)

        var errorDescription: String? {
            switch self {
            case .invalidResponse(let status):
                return "Server returned HTTP \(status)."
            case .decode(let underlying):
                return "Couldn't parse server response: \(underlying.localizedDescription)"
            case .transport(let underlying):
                return "Couldn't reach the server: \(underlying.localizedDescription). Is Tailscale connected?"
            }
        }
    }

    let baseURL: URL
    var session: URLSession = .shared

    func currentClip() async throws -> Clip {
        let url = baseURL.appending(path: "clip.json", directoryHint: .notDirectory)
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw ClientError.transport(underlying: error)
        }

        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw ClientError.invalidResponse(status: http.statusCode)
        }

        do {
            return try JSON.decoder.decode(Clip.self, from: data)
        } catch {
            throw ClientError.decode(underlying: error)
        }
    }
}
