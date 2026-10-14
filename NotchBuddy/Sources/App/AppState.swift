import Foundation
import SwiftUI
import Combine

// Integration pills — always-present, never purged
extension AgentTask {
    /// All available integration pills. Claude is always active; others are opt-in (max 4).
    static let integrationAgents: [AgentTask] = [
        AgentTask(id: "integration_claude",  name: "VS Code",   color: "#F5F6F8", state: .idle, steps: [], source: .claudeCode, isIntegration: true),
        AgentTask(id: "integration_resend",  name: "Resend",    color: "#22C55E", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_n8n",     name: "n8n",       color: "#F29B38", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_vercel",  name: "Vercel",    color: "#7C5CFF", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_github",  name: "GitHub",    color: "#F4505E", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_notion",  name: "Notion",    color: "#8C8C8C", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_calcom",  name: "Cal.com",   color: "#C9956A", state: .idle, steps: [], source: .n8n, isIntegration: true),
        AgentTask(id: "integration_stripe",  name: "Stripe",    color: "#0570DE", state: .idle, steps: [], source: .n8n, isIntegration: true),
    ]

    /// IDs that can be toggled (VS Code is always on and excluded from this list)
    static let toggleableIntegrationIds: [String] = [
        "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
        "integration_notion", "integration_calcom", "integration_stripe",
    ]

}

@MainActor
final class AppState: ObservableObject {
    static let shared = AppState()

    // Island state
    @Published var mode: IslandMode = .hidden
    @Published var view: IslandView = .overview

    // Tasks
    @Published var tasks: [AgentTask] = []
    @Published var focusId: String? = nil

    // Bot state override
    @Published var stateOverride: BotState? = nil

    // Real notch dimensions (set by IslandWindowController on launch)
    var notchWidth:  CGFloat = IslandConst.notchWidth
    var notchHeight: CGFloat = IslandConst.notchHeight

    // Last app active before NotchBuddy (for window context capture)
    var lastExternalApp: NSRunningApplication? = nil

    // Bot drag-attach state (hides original bot while ghost follows cursor)
    @Published var isDraggingBot: Bool = false

    // Mouse tracking
    var mousePosition: CGPoint = .zero
    var lastMouseMove: Date = .now
    var lastActivity: Date = .now
    var isPresent: Bool = true

    // Pinned (alerts that stay open, never auto-close)
    var isPinned: Bool = false

    // Upload progress (0-1) — set to 1.0 only at completion; animation is time-based
    @Published var uploadProgress: Double = 0

    // Upload animation timing (non-published — TimelineViews read these directly)
    var uploadStartTime: Date?
    var uploadDuration: Double = 2.4

    // File drag-over state (mailbox morph glow + mouth spring)
    @Published var fileDragOver: Bool = false

    // Sound enabled — persisted
    @Published var soundEnabled: Bool = true {
        didSet { UserDefaults.standard.set(soundEnabled, forKey: "soundEnabled") }
    }

    // Sound volume (0–0.2) — persisted, synced to SoundEngine
    @Published var soundVolume: Double = 0.12 {
        didSet {
            UserDefaults.standard.set(soundVolume, forKey: "soundVolume")
            SoundEngine.shared.volume = Float(soundVolume)
        }
    }

    // Context for prompt (window attach / file)
    @Published var promptContext: PromptContext? = nil

    // Dropped file (set during upload flow)
    @Published var droppedFile: DroppedFile? = nil

    // Short note message (shown in NoteView)
    @Published var noteMessage: String? = nil

    // Display style (notch island / thin top strip) — persisted
    @Published var displayStyle: DisplayStyle = .notch {
        didSet { UserDefaults.standard.set(displayStyle.rawValue, forKey: "displayStyle") }
    }

    // Auto-close delay — persisted
    @Published var autoCloseInterval: TimeInterval = 15 {
        didSet { UserDefaults.standard.set(autoCloseInterval, forKey: "autoCloseInterval") }
    }

    // Absence interval — persisted
    var absenceInterval: TimeInterval = 3 * 60 {
        didSet { UserDefaults.standard.set(absenceInterval, forKey: "absenceInterval") }
    }

    // Greeting threshold — how long hidden before greeting on reappear (default 2 min)
    var greetThresholdSeconds: TimeInterval = 120 {
        didSet { UserDefaults.standard.set(greetThresholdSeconds, forKey: "greetThreshold") }
    }

    // Hotkey to show island (e.g. ⌘⇧N)
    @Published var hotkeyEnabled: Bool = false {
        didSet { UserDefaults.standard.set(hotkeyEnabled, forKey: "hotkeyEnabled") }
    }
    var hotkeyFlags: UInt = NSEvent.ModifierFlags([.command, .shift]).rawValue {
        didSet { UserDefaults.standard.set(Int(hotkeyFlags), forKey: "hotkeyFlags") }
    }
    var hotkeyCode: UInt16 = 45 {  // 'n'
        didSet { UserDefaults.standard.set(Int(hotkeyCode), forKey: "hotkeyCode") }
    }

    // Vercel project filter — empty = watch all projects
    @Published var vercelProjectFilter: Set<String> = [] {
        didSet {
            if let data = try? JSONEncoder().encode(Array(vercelProjectFilter)) {
                UserDefaults.standard.set(data, forKey: "vercelProjectFilter")
            }
        }
    }

    // n8n workflow filter — empty = watch all workflows
    @Published var n8nWorkflowFilter: Set<String> = [] {
        didSet {
            if let data = try? JSONEncoder().encode(Array(n8nWorkflowFilter)) {
                UserDefaults.standard.set(data, forKey: "n8nWorkflowFilter")
            }
        }
    }

    // Active integration pills (VS Code excluded — always on). Max 4.
    @Published var activeIntegrations: Set<String> = ["integration_resend", "integration_n8n", "integration_vercel", "integration_github"] {
        didSet {
            if let data = try? JSONEncoder().encode(Array(activeIntegrations)) {
                UserDefaults.standard.set(data, forKey: "activeIntegrations")
            }
        }
    }

    // Pending API result
    @Published var searchResult: SearchResult? = nil

    // Vercel deployments (populated by VercelPoller)
    @Published var vercelDeployments: [VercelDeployment] = []

    // Resend emails (populated by ResendPoller)
    @Published var resendEmails: [ResendEmail] = []
    @Published var resendTotal: Int? = nil

    // GitHub stats (populated by GithubPoller)
    @Published var githubStats: GitHubStats? = nil

    // Stripe (populated by StripePoller)
    @Published var stripePayments: [StripePayment] = []
    @Published var stripeBalance: Int = 0           // raw balance in cents
    @Published var stripeDisplayBalance: Int = 0    // animated balance target
    @Published var stripeCurrency: String = "eur"
    @Published var stripeLoaded: Bool = false       // true after first successful poll
    @Published var stripeError: String? = nil      // last API error (nil = ok)

    // Cal.com (populated by CalcomPoller)
    @Published var calcomBookings: [CalcomBooking] = []
    @Published var calcomLoaded: Bool = false
    @Published var calcomError: String? = nil

    // Notion (populated by NotionPoller)
    @Published var notionPages: [NotionPage] = []
    @Published var notionLoaded: Bool = false
    @Published var notionError: String? = nil

    // Chat conversation history
    @Published var chatHistory: [ChatMessage] = []

    // Pending approval request from Claude Code hook
    @Published var pendingApproval: ApprovalInfo? = nil

    // Google Calendar (populated by GoogleCalendarService)
    @Published var calendarEvents: [CalendarEvent] = []
    @Published var calendarConnected: Bool = false
    @Published var calendarError: String? = nil
    @Published var calendarSyncedAt: Date? = nil

    // Strip style: a passing note written on the folded bar (integrations, calendar)
    @Published var stripNotice: StripStatus? = nil
    private var stripNoticeWork: DispatchWorkItem?

    // MARK: - Init (loads persisted settings)

    private init() {
        let ud = UserDefaults.standard

        if let s = ud.string(forKey: "displayStyle"), let v = DisplayStyle(rawValue: s) { displayStyle = v }
        if let v = ud.object(forKey: "soundEnabled") as? Bool   { soundEnabled = v }
        if let v = ud.object(forKey: "soundVolume")  as? Double { soundVolume  = v }
        // Migrate old 60s default → 15s
        if let v = ud.object(forKey: "autoCloseInterval") as? Double {
            autoCloseInterval = (v == 60) ? 15 : v
        }
        if let v = ud.object(forKey: "absenceInterval")   as? Double { absenceInterval   = v }
        if let v = ud.object(forKey: "greetThreshold")    as? Double { greetThresholdSeconds = v }
        if let v = ud.object(forKey: "hotkeyEnabled") as? Bool  { hotkeyEnabled = v }
        if let v = ud.object(forKey: "hotkeyFlags")   as? Int   { hotkeyFlags = UInt(v) }
        if let v = ud.object(forKey: "hotkeyCode")    as? Int   { hotkeyCode = UInt16(v) }
        if let d = ud.data(forKey: "vercelProjectFilter"),
           let a = try? JSONDecoder().decode([String].self, from: d) { vercelProjectFilter = Set(a) }
        if let d = ud.data(forKey: "n8nWorkflowFilter"),
           let a = try? JSONDecoder().decode([String].self, from: d) { n8nWorkflowFilter = Set(a) }
        if let d = ud.data(forKey: "activeIntegrations"),
           let a = try? JSONDecoder().decode([String].self, from: d) { activeIntegrations = Set(a) }

        // Sync SoundEngine volume on launch
        SoundEngine.shared.volume = Float(soundVolume)

        // Always load integration pills
        loadIntegrationTasks()
    }

    // MARK: - Computed

    var focusTask: AgentTask? {
        tasks.first { $0.id == focusId } ?? tasks.first
    }

    var effectiveState: BotState {
        stateOverride ?? focusTask?.state ?? .idle
    }

    // MARK: - Quiet strip
    //
    // Strip style, folded into the bar: nothing opens the island on its own.
    // Claude Code, integrations and the calendar write one line on the bar
    // instead, and the user opens it when they choose to.

    var quietStrip: Bool { displayStyle == .strip && mode == .hidden }

    /// What the bar says, or nil when there is nothing to say.
    var stripStatus: StripStatus? {
        guard displayStyle == .strip else { return nil }
        if let approval = pendingApproval {
            let target = approval.command == approval.tool ? approval.tool : "\(approval.tool) · \(approval.command)"
            return StripStatus(text: "Needs permission · \(target)", color: "#F5A524")
        }
        // Short-lived, so it may talk over Claude Code's running status for a moment.
        if let notice = stripNotice { return notice }
        guard let claude = tasks.first(where: { $0.id == "integration_claude" }) else { return nil }
        let step = claude.steps.last
        let project = claude.name == "VS Code" || claude.name.isEmpty ? "Claude Code" : claude.name
        func withStep(_ label: String) -> String { step.map { "\(label) · \($0)" } ?? label }
        switch claude.state {
        case .thinking:  return StripStatus(text: withStep("\(project) is thinking"), color: "#A78BFA")
        case .working:   return StripStatus(text: step.map { "\(project) · \($0)" } ?? "\(project) is working", color: "#3B9EFF")
        case .question:  return StripStatus(text: withStep("Question"), color: "#22D3EE")
        case .finished:  return StripStatus(text: withStep("\(project) finished"), color: "#34D399")
        case .error:     return StripStatus(text: withStep("\(project) stopped on an error"), color: "#F4505E")
        case .ratelimit: return StripStatus(text: "Claude Code hit a rate limit", color: "#F59E0B")
        default:         return nil
        }
    }

    /// Width the bar needs for its status line (0 = no line). The strip grows to it.
    var stripStatusWidth: CGFloat {
        guard let status = stripStatus else { return 0 }
        let font = NSFont.systemFont(ofSize: 10, weight: .medium)
        let text = (status.text as NSString).size(withAttributes: [.font: font]).width
        // dot 5 + gap 6 + side padding 2×12
        return min(IslandConst.stripStatusMaxWidth, ceil(text) + 35)
    }

    /// A passing note on the folded strip, gone after `seconds`.
    func notifyStrip(_ text: String, color: String, for seconds: TimeInterval) {
        stripNoticeWork?.cancel()
        stripNotice = StripStatus(text: text, color: color)
        let item = DispatchWorkItem { [weak self] in self?.stripNotice = nil }
        stripNoticeWork = item
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: item)
    }

    // MARK: - Task management

    func addTask(_ task: AgentTask) {
        guard !tasks.contains(where: { $0.id == task.id }) else { return }
        tasks.append(task)
        if focusId == nil { focusId = task.id }
        syncMode()
        syncView()
    }

    func removeTask(id: String) {
        tasks.removeAll { $0.id == id }
        if focusId == id { focusId = tasks.first?.id }
        syncMode()
        syncView()
    }

    func updateTask(id: String, state: BotState) {
        guard let idx = tasks.firstIndex(where: { $0.id == id }) else { return }
        tasks[idx].state = state
    }

    func setFocus(_ id: String) {
        guard let idx = tasks.firstIndex(where: { $0.id == id }) else { return }
        focusId = id
        tasks[idx].pillBadge = nil  // clear badge when user brings task to focus
    }

    func syncMode() {
        // If no tasks and not expanded/peek, go hidden
        if tasks.isEmpty && mode == .compact {
            mode = .hidden
        } else if !tasks.isEmpty && mode == .hidden && isPresent && displayStyle == .notch {
            // Strip style: the peek only ever opens on hover (driven by the FSM)
            mode = .compact
        }
    }

    func syncView() {
        guard mode == .expanded else { return }
        if view == .empty && !tasks.isEmpty { view = .overview }
        else if view == .overview && tasks.isEmpty { view = .empty }
    }

    /// Load integration pills respecting activeIntegrations. VS Code always loads. Safe to call multiple times.
    func loadIntegrationTasks() {
        for task in AgentTask.integrationAgents {
            let shouldLoad = task.id == "integration_claude" || activeIntegrations.contains(task.id)
            let loaded = tasks.contains(where: { $0.id == task.id })
            if shouldLoad && !loaded { tasks.append(task) }
            if !shouldLoad && loaded { tasks.removeAll { $0.id == task.id } }
        }
        if focusId == nil { focusId = "integration_claude" }
        syncMode()
    }

    /// Toggle an integration pill on/off. VS Code cannot be toggled. Max 4 active at once.
    func toggleIntegration(_ id: String) {
        guard id != "integration_claude" else { return }
        if activeIntegrations.contains(id) {
            activeIntegrations.remove(id)
            tasks.removeAll { $0.id == id }
            if focusId == id { focusId = "integration_claude" }
        } else {
            guard activeIntegrations.count < 4 else { return }
            activeIntegrations.insert(id)
            if let task = AgentTask.integrationAgents.first(where: { $0.id == id }),
               !tasks.contains(where: { $0.id == id }) {
                tasks.append(task)
            }
        }
        syncMode()
    }

}

// MARK: - Supporting types

/// One line on the folded strip: what it says and the colour of its dot.
struct StripStatus: Equatable {
    let text: String
    let color: String
}

enum PromptContext {
    case window(appName: String, title: String, url: String?)
    case file(name: String, fileURL: URL?)
}

struct DroppedFile {
    var url: URL
    var name: String
}

struct SearchResult {
    var title: String
    var items: [ResultItem]
    var note: String?
}

struct ResultItem {
    var label: String
    var detail: String
    var url: String?
}

// MARK: - Vercel

struct VercelDeployment: Identifiable {
    let id: String
    let projectName: String
    let url: String
    let state: String        // "READY", "ERROR", "CANCELED"
    let createdAt: Date
    let commitMessage: String?
    let branch: String?

    var isSuccess: Bool { state == "READY" }
    var statusLabel: String { isSuccess ? "Ready" : (state == "CANCELED" ? "Canceled" : "Error") }
    var timeAgo: String {
        let diff = Date().timeIntervalSince(createdAt)
        if diff < 60    { return "just now" }
        if diff < 3600  { return "\(Int(diff/60))m" }
        if diff < 86400 { return "\(Int(diff/3600))h" }
        return "\(Int(diff/86400))d"
    }
}

// MARK: - Resend

struct ResendEmail: Identifiable {
    let id: String
    let to: [String]
    let subject: String
    let createdAt: Date
    let lastEvent: String   // "delivered", "bounced", "complained", "opened", etc.

    var recipientShort: String {
        guard let first = to.first else { return "?" }
        return first.components(separatedBy: "@").first ?? first
    }
    var timeAgo: String {
        let diff = Date().timeIntervalSince(createdAt)
        if diff < 60    { return "just now" }
        if diff < 3600  { return "\(Int(diff/60))m" }
        if diff < 86400 { return "\(Int(diff/3600))h" }
        return "\(Int(diff/86400))d"
    }
    var isDelivered: Bool { lastEvent == "delivered" }
}

// MARK: - GitHub

struct GitHubStats {
    let totalRepos: Int
    let totalStars: Int
}

// MARK: - Stripe

struct StripePayment: Identifiable, Equatable {
    let id: String
    let amount: Int         // in cents/smallest unit
    let currency: String
    let description: String?
    let createdAt: Date
    let status: String      // "succeeded", "pending", "failed"

    var amountFormatted: String { String(format: "%.2f", Double(amount) / 100.0) }
    var isSuccess: Bool { status == "succeeded" }
    var timeAgo: String {
        let diff = Date().timeIntervalSince(createdAt)
        if diff < 60    { return "just now" }
        if diff < 3600  { return "\(Int(diff/60))m" }
        if diff < 86400 { return "\(Int(diff/3600))h" }
        return "\(Int(diff/86400))d"
    }
}

// MARK: - Cal.com

struct CalcomBooking: Identifiable, Equatable {
    let id: Int
    let title: String
    let startTime: Date
    let endTime: Date
    let status: String
    let attendeeName: String?
    let attendeeEmail: String?
    let attendeeNotes: String?

    var isActive: Bool { status == "ACCEPTED" || status == "PENDING" }
    var timeLabel: String {
        let f = DateFormatter(); f.dateFormat = "HH:mm"; return f.string(from: startTime)
    }
    var dayKey: String {
        let c = Calendar.current.dateComponents([.year, .month, .day], from: startTime)
        return "\(c.year!)-\(String(format: "%02d", c.month!))-\(String(format: "%02d", c.day!))"
    }
}

// MARK: - Notion

struct NotionPage: Identifiable {
    let id: String
    let title: String
    let emoji: String?
    let lastEditedAt: Date
    let url: String

    var timeAgo: String {
        let diff = Date().timeIntervalSince(lastEditedAt)
        if diff < 60 { return "now" }
        if diff < 3600 { return "\(Int(diff/60))m" }
        if diff < 86400 { return "\(Int(diff/3600))h" }
        return "\(Int(diff/86400))d"
    }
}

// MARK: - Chat

enum ChatRole { case user, assistant }

struct ChatMessage: Identifiable {
    let id = UUID()
    let role: ChatRole
    let content: String
}
