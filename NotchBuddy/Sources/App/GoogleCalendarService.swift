import AppKit
import CryptoKit
import Foundation
import Security

// MARK: - Google Calendar
//
// Read-only sync of the week ahead, plus the meeting heads-up.
//
// Sign-in is Google's "installed app" flow: the browser opens the consent page,
// Google redirects to a one-shot listener on 127.0.0.1, and the code is
// exchanged with PKCE. The user brings their own OAuth client (Google Cloud →
// Credentials → Desktop app), so nothing goes through a server of ours.
// Client ID, client secret and refresh token live in the Keychain; the
// short-lived access token only in memory.

struct CalendarEvent: Identifiable, Equatable, Sendable {
    let id: String            // "<event id>@<calendar id>" — unique across calendars
    let calendar: String
    let color: String
    let title: String
    let start: Date
    let end: Date
    let allDay: Bool
    let location: String?
    let meetURL: String?
    let htmlLink: String?
}

struct CalendarError: LocalizedError, Sendable {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

@MainActor
final class GoogleCalendarService {
    static let shared = GoogleCalendarService()

    nonisolated static let clientIdKey     = "gcal-client-id"
    nonisolated static let clientSecretKey = "gcal-client-secret"
    /// Written only by the sign-in below.
    nonisolated static let refreshKey      = "gcal-refresh-token"

    private static let authURL   = "https://accounts.google.com/o/oauth2/v2/auth"
    private static let tokenURL  = "https://oauth2.googleapis.com/token"
    private static let revokeURL = "https://oauth2.googleapis.com/revoke"
    private static let api       = "https://www.googleapis.com/calendar/v3"
    private static let scope     = "https://www.googleapis.com/auth/calendar.readonly"

    private static let syncEvery: TimeInterval = 120
    private static let headsUp: TimeInterval   = 5 * 60
    /// One request each per sync; anything beyond is skipped.
    private static let maxCalendars = 12

    private var accessToken: String?
    private var accessExpiry: Date = .distantPast
    private var timer: Timer?
    private(set) var signingIn = false

    private var reminderWork: DispatchWorkItem?
    /// "<id>@<start>" of every event already announced, so a resync can't repeat one.
    private var announced = Set<String>()

    private init() {}

    var isConnected: Bool { KeychainStore.shared.get(Self.refreshKey) != nil }
    var hasClient: Bool { !(KeychainStore.shared.get(Self.clientIdKey) ?? "").isEmpty }

    // MARK: - Lifecycle

    func start() {
        AppState.shared.calendarConnected = isConnected
        timer = Timer.scheduledTimer(withTimeInterval: Self.syncEvery, repeats: true) { _ in
            Task { @MainActor in await GoogleCalendarService.shared.sync() }
        }
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(4))
            await GoogleCalendarService.shared.sync()
        }
    }

    // MARK: - Sign-in

    /// Opens Google's consent page and waits for the browser to come back.
    func connect() async throws {
        guard let clientId = KeychainStore.shared.get(Self.clientIdKey), !clientId.isEmpty else {
            throw CalendarError("Save your OAuth client ID first.")
        }
        let secret = KeychainStore.shared.get(Self.clientSecretKey) ?? ""
        guard !signingIn else { throw CalendarError("A sign-in is already waiting in your browser.") }
        signingIn = true
        defer { signingIn = false }

        let verifier  = Self.randomToken(48)
        let challenge = Self.base64url(Data(SHA256.hash(data: Data(verifier.utf8))))
        let state     = Self.randomToken(16)

        let (fd, port) = try LoopbackRedirect.open()
        let redirect = "http://127.0.0.1:\(port)"

        var comps = URLComponents(string: Self.authURL)!
        comps.queryItems = [
            .init(name: "client_id", value: clientId),
            .init(name: "redirect_uri", value: redirect),
            .init(name: "response_type", value: "code"),
            .init(name: "scope", value: Self.scope),
            .init(name: "code_challenge", value: challenge),
            .init(name: "code_challenge_method", value: "S256"),
            .init(name: "state", value: state),
            .init(name: "access_type", value: "offline"),
            // Without it Google only hands out a refresh token the very first time.
            .init(name: "prompt", value: "consent"),
        ]
        guard let url = comps.url else { close(fd); throw CalendarError("Bad client ID.") }
        NSWorkspace.shared.open(url)

        let code = try await Task.detached {
            defer { close(fd) }
            return try LoopbackRedirect.waitForCode(fd: fd, state: state, timeout: 300)
        }.value

        let json = try await postForm(Self.tokenURL, [
            ("code", code),
            ("client_id", clientId),
            ("client_secret", secret),
            ("redirect_uri", redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ], failure: "Google refused the sign-in")
        guard let refresh = json["refresh_token"] as? String else {
            throw CalendarError("Google sent no refresh token — remove Coucou from your Google account's third-party access and try again.")
        }
        KeychainStore.shared.set(Self.refreshKey, value: refresh)
        accessToken = json["access_token"] as? String
        accessExpiry = Date().addingTimeInterval(Double((json["expires_in"] as? Int ?? 3600) - 60))
        await sync()
    }

    /// Forgets the account here and tells Google to revoke the grant.
    func disconnect() async {
        if let token = KeychainStore.shared.get(Self.refreshKey) {
            _ = try? await postForm(Self.revokeURL, [("token", token)], failure: "")
        }
        KeychainStore.shared.remove(Self.refreshKey)
        accessToken = nil
        let state = AppState.shared
        state.calendarConnected = false
        state.calendarEvents = []
        state.calendarError = nil
        state.calendarSyncedAt = nil
        scheduleReminder()
    }

    // MARK: - Sync

    func sync() async {
        let state = AppState.shared
        guard isConnected else {
            state.calendarConnected = false
            state.calendarEvents = []
            return
        }
        do {
            let events = try await fetchEvents()
            state.calendarConnected = true
            state.calendarEvents = events
            state.calendarError = nil
            state.calendarSyncedAt = Date()
        } catch {
            // Keep what is on screen; just say what went wrong.
            state.calendarConnected = isConnected
            state.calendarError = error.localizedDescription
            if !isConnected { state.calendarEvents = [] }
        }
        scheduleReminder()
    }

    private func token() async throws -> String {
        if let t = accessToken, Date() < accessExpiry { return t }
        guard let refresh = KeychainStore.shared.get(Self.refreshKey) else { throw CalendarError("Not connected.") }
        guard let clientId = KeychainStore.shared.get(Self.clientIdKey) else {
            throw CalendarError("The OAuth client ID is missing.")
        }
        let secret = KeychainStore.shared.get(Self.clientSecretKey) ?? ""
        do {
            let json = try await postForm(Self.tokenURL, [
                ("client_id", clientId),
                ("client_secret", secret),
                ("refresh_token", refresh),
                ("grant_type", "refresh_token"),
            ], failure: "Could not refresh the Google token")
            guard let t = json["access_token"] as? String else { throw CalendarError("Google sent no access token.") }
            accessToken = t
            accessExpiry = Date().addingTimeInterval(Double((json["expires_in"] as? Int ?? 3600) - 60))
            return t
        } catch let e as CalendarError where e.message.contains("invalid_grant") {
            // Revoked from the Google account, or expired (apps still in "Testing"
            // get seven days): the token is dead, stop pretending to be connected.
            KeychainStore.shared.remove(Self.refreshKey)
            throw CalendarError("Google access expired or was revoked — connect again in Settings.")
        }
    }

    /// Events from every calendar ticked in Google Calendar, from a day and a half
    /// ago (so "today" is always covered) to eight days ahead.
    private func fetchEvents() async throws -> [CalendarEvent] {
        let t = try await token()

        var listComps = URLComponents(string: "\(Self.api)/users/me/calendarList")!
        listComps.queryItems = [.init(name: "minAccessRole", value: "reader")]
        let list = try await getJSON(listComps.url!, token: t)
        let calendars: [(id: String, name: String, color: String)] = (list["items"] as? [[String: Any]] ?? [])
            .filter { ($0["selected"] as? Bool ?? false) || ($0["primary"] as? Bool ?? false) }
            .compactMap { (c: [String: Any]) -> (id: String, name: String, color: String)? in
                guard let id = c["id"] as? String else { return nil }
                let name = c["summaryOverride"] as? String ?? c["summary"] as? String ?? "Calendar"
                return (id: id, name: name, color: c["backgroundColor"] as? String ?? "#4285F4")
            }
            .prefix(Self.maxCalendars)
            .map { $0 }

        let iso = ISO8601DateFormatter()
        let timeMin = iso.string(from: Date().addingTimeInterval(-36 * 3600))
        let timeMax = iso.string(from: Date().addingTimeInterval(8 * 86400))

        var pathAllowed = CharacterSet.urlPathAllowed
        pathAllowed.remove(charactersIn: "/")

        var events: [CalendarEvent] = []
        for cal in calendars {
            let encoded = cal.id.addingPercentEncoding(withAllowedCharacters: pathAllowed) ?? cal.id
            guard var comps = URLComponents(string: "\(Self.api)/calendars/\(encoded)/events") else { continue }
            comps.queryItems = [
                .init(name: "timeMin", value: timeMin),
                .init(name: "timeMax", value: timeMax),
                .init(name: "singleEvents", value: "true"),
                .init(name: "orderBy", value: "startTime"),
                .init(name: "maxResults", value: "100"),
            ]
            guard let url = comps.url else { continue }
            let json = try await getJSON(url, token: t)
            for item in json["items"] as? [[String: Any]] ?? [] {
                if let e = Self.parseEvent(item, calendarId: cal.id, calendar: cal.name, color: cal.color) {
                    events.append(e)
                }
            }
        }
        return events.sorted { $0.start == $1.start ? ($0.allDay && !$1.allDay) : $0.start < $1.start }
    }

    private static func parseEvent(_ e: [String: Any], calendarId: String, calendar: String, color: String) -> CalendarEvent? {
        if e["status"] as? String == "cancelled" { return nil }
        // Invitations you said no to don't belong on your day.
        let declined = (e["attendees"] as? [[String: Any]] ?? []).contains {
            ($0["self"] as? Bool ?? false) && $0["responseStatus"] as? String == "declined"
        }
        if declined { return nil }
        guard let id = e["id"] as? String, let startInfo = when(e["start"]) else { return nil }
        let start = startInfo.0
        let allDay = startInfo.1
        let end = when(e["end"])?.0 ?? start
        let entries = (e["conferenceData"] as? [String: Any])?["entryPoints"] as? [[String: Any]] ?? []
        let video = entries.first(where: { $0["entryPointType"] as? String == "video" })
        let meet: String? = (e["hangoutLink"] as? String) ?? (video?["uri"] as? String)
        let title = (e["summary"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "(No title)"
        return CalendarEvent(id: "\(id)@\(calendarId)", calendar: calendar, color: color, title: title,
                             start: start, end: end, allDay: allDay,
                             location: e["location"] as? String, meetURL: meet,
                             htmlLink: e["htmlLink"] as? String)
    }

    /// `{dateTime}` → an instant; `{date}` (all-day) → local midnight of that day.
    private static func when(_ raw: Any?) -> (Date, Bool)? {
        guard let obj = raw as? [String: Any] else { return nil }
        if let s = obj["dateTime"] as? String {
            let f = ISO8601DateFormatter()
            if let d = f.date(from: s) { return (d, false) }
            f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            return f.date(from: s).map { ($0, false) }
        }
        if let s = obj["date"] as? String {
            let f = DateFormatter()
            f.calendar = Calendar(identifier: .gregorian)
            f.locale = Locale(identifier: "en_US_POSIX")
            f.timeZone = .current
            f.dateFormat = "yyyy-MM-dd"
            return f.date(from: s).map { ($0, true) }
        }
        return nil
    }

    // MARK: - Meeting heads-up
    //
    // Five minutes before an event starts. One timer for the next event only —
    // nothing ticks in between.

    private func scheduleReminder() {
        reminderWork?.cancel()
        reminderWork = nil
        let now = Date()
        guard let next = AppState.shared.calendarEvents
            .filter({ !$0.allDay && $0.start > now && !announced.contains("\($0.id)@\($0.start.timeIntervalSince1970)") })
            .min(by: { $0.start < $1.start }) else { return }
        let item = DispatchWorkItem {
            Task { @MainActor in GoogleCalendarService.shared.announce(next) }
        }
        reminderWork = item
        let delay = max(0, next.start.timeIntervalSinceNow - Self.headsUp)
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
    }

    private func announce(_ event: CalendarEvent) {
        announced.insert("\(event.id)@\(event.start.timeIntervalSince1970)")
        let state = AppState.shared
        if state.quietStrip {
            // Folded into the thin strip: say it there, until the meeting starts.
            SoundEngine.shared.play("pop")
            let minutes = max(1, Int((event.start.timeIntervalSinceNow / 60).rounded()))
            state.notifyStrip("In \(minutes) min · \(event.title)", color: event.color,
                              for: max(10, event.start.timeIntervalSinceNow))
        } else if state.mode != .expanded {
            // Never in the middle of something else.
            SoundEngine.shared.play("pop")
            NotificationCenter.default.post(name: .hookExpand, object: IslandView.calendar)
        }
        scheduleReminder()
    }

    // MARK: - HTTP

    private func getJSON(_ url: URL, token: String) async throws -> [String: Any] {
        var req = URLRequest(url: url, timeoutInterval: 15)
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: req)
        let code = (response as? HTTPURLResponse)?.statusCode ?? 0
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        if (200..<300).contains(code) { return json }
        if code == 401 { accessToken = nil }
        throw CalendarError(Self.googleError(json, fallback: "Google Calendar error \(code)"))
    }

    private func postForm(_ endpoint: String, _ pairs: [(String, String)], failure: String) async throws -> [String: Any] {
        var req = URLRequest(url: URL(string: endpoint)!, timeoutInterval: 15)
        req.httpMethod = "POST"
        req.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        req.httpBody = Data(pairs.map { "\(Self.formEncode($0.0))=\(Self.formEncode($0.1))" }
            .joined(separator: "&").utf8)
        let (data, response) = try await URLSession.shared.data(for: req)
        let code = (response as? HTTPURLResponse)?.statusCode ?? 0
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        guard (200..<300).contains(code) else {
            // Keep the raw OAuth error code in the message: token() looks for invalid_grant.
            let raw = json["error"] as? String
            let base = Self.googleError(json, fallback: failure)
            throw CalendarError(raw.map { base.contains($0) ? base : "\(base) (\($0))" } ?? base)
        }
        return json
    }

    private static func googleError(_ json: [String: Any], fallback: String) -> String {
        let detail = json["error_description"] as? String
            ?? (json["error"] as? [String: Any])?["message"] as? String
            ?? json["error"] as? String
        guard let detail else { return fallback }
        return fallback.isEmpty ? detail : "\(fallback): \(detail)"
    }

    // MARK: - Small helpers

    private static let formAllowed = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    private static func formEncode(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: formAllowed) ?? s
    }

    private static func randomToken(_ count: Int) -> String {
        var bytes = [UInt8](repeating: 0, count: count)
        _ = SecRandomCopyBytes(kSecRandomDefault, count, &bytes)
        return base64url(Data(bytes))
    }

    private static func base64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

// MARK: - Loopback redirect listener
//
// A blocking BSD socket on 127.0.0.1, served from a detached task: it answers
// the single redirect Google makes and nothing else. Same approach as the hook
// socket in HookServer. (The App Store build needs network.server for this.)

enum LoopbackRedirect {
    /// Binds a free port on 127.0.0.1. Returns the listening fd and its port.
    static func open() throws -> (Int32, UInt16) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw CalendarError("Could not open the sign-in listener.") }
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 4) == 0 else {
            close(fd)
            throw CalendarError("Could not open the sign-in listener.")
        }
        var actual = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &actual) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) }
        }
        return (fd, UInt16(bigEndian: actual.sin_port))
    }

    /// Serves the redirect and returns the `code` it carries. Anything else that
    /// hits the port (a favicon request) gets a 404 and is ignored.
    static func waitForCode(fd: Int32, state: String, timeout: TimeInterval) throws -> String {
        let deadline = Date().addingTimeInterval(timeout)
        while true {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw CalendarError("Sign-in timed out — try again.") }
            var pfd = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            let ready = poll(&pfd, 1, Int32(min(remaining, 1) * 1000))
            if ready == 0 { continue }
            guard ready > 0 else { throw CalendarError("The sign-in listener stopped.") }

            let client = accept(fd, nil, nil)
            guard client >= 0 else { continue }
            defer { close(client) }
            var tv = timeval(tv_sec: 5, tv_usec: 0)
            setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

            var buffer = [UInt8](repeating: 0, count: 8192)
            var total = 0
            while total < buffer.count {
                let n = buffer.withUnsafeMutableBytes { raw in
                    recv(client, raw.baseAddress! + total, raw.count - total, 0)
                }
                if n <= 0 { break }
                total += n
                if String(decoding: buffer[0..<total], as: UTF8.self).contains("\r\n\r\n") { break }
            }
            let head = String(decoding: buffer[0..<total], as: UTF8.self)
            let target = head.components(separatedBy: "\r\n").first?
                .split(separator: " ").dropFirst().first.map(String.init) ?? "/"
            guard let comps = URLComponents(string: "http://127.0.0.1\(target)") else {
                respond(client, 400, "Bad request")
                continue
            }
            let items = comps.queryItems ?? []
            let param: (String) -> String? = { name in items.first { $0.name == name }?.value }

            if let error = param("error") {
                respond(client, 200, "Google Calendar was not connected. You can close this tab.")
                throw CalendarError(error == "access_denied" ? "Access was declined." : "Google said: \(error)")
            }
            guard let code = param("code") else {
                respond(client, 404, "Not found")
                continue
            }
            guard param("state") == state else {
                respond(client, 400, "This sign-in link is stale. Start again from Coucou.")
                throw CalendarError("The sign-in answer did not match — try again.")
            }
            respond(client, 200, "Coucou is connected to Google Calendar. You can close this tab.")
            return code
        }
    }

    private static func respond(_ client: Int32, _ status: Int, _ message: String) {
        let reason = status == 200 ? "OK" : status == 400 ? "Bad Request" : "Not Found"
        let body = "<!doctype html><meta charset=utf-8><title>Coucou</title>"
            + "<body style=\"font:15px -apple-system,system-ui;background:#0b0c0e;color:#f5f6f8;"
            + "display:grid;place-items:center;height:90vh\"><p>\(message)</p></body>"
        let bodyData = Data(body.utf8)
        let headData = Data(("HTTP/1.1 \(status) \(reason)\r\nContent-Type: text/html; charset=utf-8\r\n"
            + "Content-Length: \(bodyData.count)\r\nConnection: close\r\n\r\n").utf8)
        let reply = headData + bodyData
        reply.withUnsafeBytes { raw in
            var sent = 0
            while sent < raw.count {
                let n = send(client, raw.baseAddress! + sent, raw.count - sent, 0)
                if n <= 0 { break }
                sent += n
            }
        }
    }
}
