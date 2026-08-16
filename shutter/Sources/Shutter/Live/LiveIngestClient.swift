import Foundation

// LiveIngestClient — WebSocket ingest to shutter-relay (video-only, no audio).
// Sends binary fMP4: first message is the init segment (ftyp+moov), subsequent
// messages are moof+mdat fragments (avc1.42E01E).

final class LiveIngestClient {
    let streamID: String
    let wsURL: URL
    var token: String?
    private var task: URLSessionWebSocketTask?
    private let session = URLSession(configuration: .default)
    private var isConnected = false

    init(streamID: String, wsURL: URL) {
        self.streamID = streamID
        self.wsURL = wsURL
    }

    func connect() async throws {
        var request = URLRequest(url: wsURL)
        if let token = token, !token.isEmpty, !wsURL.absoluteString.contains("token=") {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        task = session.webSocketTask(with: request)
        task?.resume()
        isConnected = true
        // Hello for observability — relay ignores text apart from "end".
        let hello: [String: Any] = ["type": "hello", "id": streamID, "videoOnly": true, "codec": "avc1.42E01E"]
        if let data = try? JSONSerialization.data(withJSONObject: hello),
           let txt = String(data: data, encoding: .utf8) {
            try? await task?.send(.string(txt))
        }
        Log.info("live ingest connected — \(wsURL)")
    }

    func sendInit(_ data: Data) {
        guard isConnected, let task = task else { return }
        Task { try? await task.send(.data(data)) }
    }

    func sendFragment(_ data: Data) {
        guard isConnected, let task = task else { return }
        Task { try? await task.send(.data(data)) }
    }

    func disconnect() async {
        guard let task = task else { return }
        try? await task.send(.string("{\"type\":\"end\"}"))
        task.cancel(with: .goingAway, reason: nil)
        self.task = nil
        isConnected = false
        Log.info("live ingest disconnected")
    }
}
