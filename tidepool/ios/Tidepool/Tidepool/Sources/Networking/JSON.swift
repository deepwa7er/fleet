import Foundation

enum JSON {
    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let raw = try container.decode(String.self)
            if let date = RFC3339.parse(raw) {
                return date
            }
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "invalid RFC3339 timestamp: \(raw)"
            )
        }
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        return e
    }()
}

private enum RFC3339 {
    static let withFraction: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    static let plain: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    /// `ISO8601DateFormatter` only accepts exactly zero fractional digits
    /// (without `.withFractionalSeconds`) or exactly three (with it).
    /// Go's `time.Time` encoder emits 0..9 fractional digits depending on
    /// the timestamp, so normalize to one of those two shapes first.
    static func parse(_ raw: String) -> Date? {
        guard let dotRange = raw.range(of: ".") else {
            return plain.date(from: raw)
        }
        let fractionStart = dotRange.upperBound
        guard let suffixStart = raw[fractionStart...].firstIndex(where: { !$0.isNumber }) else {
            return nil
        }
        let digits = raw[fractionStart..<suffixStart]
        let clamped = String(digits.prefix(3)).padding(toLength: 3, withPad: "0", startingAt: 0)
        let normalized = String(raw[..<fractionStart]) + clamped + String(raw[suffixStart...])
        return withFraction.date(from: normalized)
    }
}
