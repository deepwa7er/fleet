import Foundation

struct Clip: Codable, Equatable, Hashable {
    let text: String
    let updatedAt: Date
    let updatedBy: String

    enum CodingKeys: String, CodingKey {
        case text
        case updatedAt = "updated_at"
        case updatedBy = "updated_by"
    }
}
