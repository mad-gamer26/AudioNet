import Foundation

/// A device's platform in words ("Windows", "Mac", "iPhone"…), or empty
/// when the server does not know it.
func platformName(_ platform: String) -> String {
    switch platform {
    case "windows": return "Windows"
    case "macos": return "Mac"
    case "ios": return "iPhone"
    case "android": return "Android"
    case "linux": return "Linux"
    case "web": return "Web browser"
    case "": return ""
    default: return platform.prefix(1).uppercased() + platform.dropFirst()
    }
}

/// A device in a list: "Studio PC, Windows". Every listed device is
/// visible, so there is no state to add.
func deviceLabel(_ d: Device) -> String {
    let p = platformName(d.platform)
    return p.isEmpty ? d.name : "\(d.name), \(p)"
}

func sourceLabel(_ s: Source) -> String {
    "\(s.name) (\(s.isInput ? "input" : "what it plays")\(s.isDefault ? ", default" : ""))"
}

func outputLabel(_ o: Output) -> String {
    o.isDefault ? "\(o.name) (default)" : o.name
}
