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

    func postClip(text: String) async throws {
        let url = baseURL.appending(path: "clip", directoryHint: .notDirectory)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/x-www-form-urlencoded; charset=utf-8", forHTTPHeaderField: "Content-Type")

        var components = URLComponents()
        components.queryItems = [URLQueryItem(name: "text", value: text)]
        request.httpBody = (components.percentEncodedQuery ?? "").data(using: .utf8)

        let response: URLResponse
        do {
            (_, response) = try await session.data(for: request)
        } catch {
            throw ClientError.transport(underlying: error)
        }

        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw ClientError.invalidResponse(status: http.statusCode)
        }
    }
}
