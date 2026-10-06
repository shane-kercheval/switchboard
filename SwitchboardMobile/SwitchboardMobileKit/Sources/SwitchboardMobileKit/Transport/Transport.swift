import Foundation

/// One encrypted record addressed through the relay. The relay stamps `from`
/// with the sender's registered device id; a client never sets it.
struct Frame: Equatable, Sendable {
    var to: String
    var from: String?
    var record: Data
}

enum TransportStatus: Equatable, Sendable {
    case disabled
    case connecting
    case connected
    case relayUnreachable(since: Date)
}

protocol Transport: Sendable {
    func send(_ frame: Frame) async throws
    var inbound: AsyncStream<Frame> { get }
    var status: AsyncStream<TransportStatus> { get }
}

protocol RequestBroker: Sendable {
    func request<R: Decodable & Sendable>(_ type: String, _ payload: any Encodable & Sendable) async throws -> R
}
