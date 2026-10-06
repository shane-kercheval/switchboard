import Foundation
import Testing

/// Reads each configuration's Info.plist through the path the Xcode project
/// assigns it, so these tests follow what a build of that configuration ships.
struct InfoPlistTests {
    @Test(arguments: ["Debug", "Release"])
    func carriesPrivacyUsageDescriptions(configuration: String) throws {
        let plist = try AppInfoPlist(configuration: configuration)
        for key in ["NSCameraUsageDescription", "NSFaceIDUsageDescription"] {
            let description = try #require(plist.values[key] as? String, "\(configuration) lacks \(key)")
            #expect(!description.isEmpty)
        }
    }

    @Test func releaseCarriesNoLocalNetworkExceptions() throws {
        let plist = try AppInfoPlist(configuration: "Release")
        #expect(plist.values["NSAppTransportSecurity"] == nil)
        #expect(plist.values["NSLocalNetworkUsageDescription"] == nil)
    }

    @Test func debugAllowsTheLocalRelay() throws {
        let plist = try AppInfoPlist(configuration: "Debug")
        let transportSecurity = try #require(plist.values["NSAppTransportSecurity"] as? [String: Any])
        #expect(transportSecurity["NSAllowsLocalNetworking"] as? Bool == true)
        #expect(plist.values["NSLocalNetworkUsageDescription"] is String)
    }
}

private struct AppInfoPlist {
    let values: [String: Any]

    private static let appTargetName = "SwitchboardMobile"

    private static let projectDirectory = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()

    init(configuration: String) throws {
        let projectFile = Self.projectDirectory
            .appending(path: "SwitchboardMobile.xcodeproj/project.pbxproj")
        let project = try Self.propertyList(at: projectFile)
        let objects = try #require(project["objects"] as? [String: [String: Any]])

        let target = try #require(objects.values.first {
            $0["isa"] as? String == "PBXNativeTarget" && $0["name"] as? String == Self.appTargetName
        })
        let configurationListID = try #require(target["buildConfigurationList"] as? String)
        let configurationList = try #require(objects[configurationListID])
        let configurationIDs = try #require(configurationList["buildConfigurations"] as? [String])
        let buildConfiguration = try #require(
            configurationIDs.compactMap { objects[$0] }.first { $0["name"] as? String == configuration }
        )
        let settings = try #require(buildConfiguration["buildSettings"] as? [String: Any])
        let infoPlistPath = try #require(settings["INFOPLIST_FILE"] as? String)

        values = try Self.propertyList(at: Self.projectDirectory.appending(path: infoPlistPath))
    }

    private static func propertyList(at url: URL) throws -> [String: Any] {
        let data = try Data(contentsOf: url)
        return try #require(PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any])
    }
}
