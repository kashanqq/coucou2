import Foundation
import Security

// MARK: - Keychain helpers

enum Keychain {
    static let service = "fr.louisraille.NotchBuddy"

    static func save(key: String, value: String) {
        guard let data = value.data(using: .utf8) else { return }
        // Delete existing item first (update pattern)
        let lookup: [String: Any] = [
            kSecClass as String:       kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(lookup as CFDictionary)
        // Add with strictest access control:
        // WhenUnlockedThisDeviceOnly = accessible only while Mac is unlocked,
        // never synced to iCloud, never migrated to another device.
        let item: [String: Any] = [
            kSecClass as String:            kSecClassGenericPassword,
            kSecAttrService as String:      service,
            kSecAttrAccount as String:      key,
            kSecValueData as String:        data,
            kSecAttrAccessible as String:   kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            kSecAttrSynchronizable as String: kCFBooleanFalse!,
        ]
        SecItemAdd(item as CFDictionary, nil)
    }

    static func load(key: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String:       kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String:  true,
            kSecMatchLimit as String:  kSecMatchLimitOne,
        ]
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func delete(key: String) {
        let query: [String: Any] = [
            kSecClass as String:       kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

// MARK: - Keychain cache (reads each key ONCE at launch; all subsequent access via dict)

final class KeychainStore: @unchecked Sendable {
    static let shared = KeychainStore()
    private var cache: [String: String] = [:]
    private let lock = NSLock()

    private static let allKeys = [
        "anthropic-api-key", "openai-compat-key",
        "resend-api-key", "resend-from",
        "n8n-url", "n8n-api-key",
        "vercel-token",
        "github-token",
        "stripe-api-key",
        "calcom-api-key",
        "notion-api-key",
        "gcal-client-id", "gcal-client-secret", "gcal-refresh-token",
    ]

    private init() {
        // Called once, on main thread (AppDelegate triggers shared at launch).
        for key in Self.allKeys {
            if let v = Keychain.load(key: key) { cache[key] = v }
        }
    }

    /// Thread-safe read — never touches the Keychain.
    func get(_ key: String) -> String? {
        lock.withLock { cache[key] }
    }

    /// Updates cache + persists to Keychain.
    func set(_ key: String, value: String) {
        lock.withLock { cache[key] = value }
        Keychain.save(key: key, value: value)
    }

    /// Removes from cache + Keychain only if the key was previously set.
    func remove(_ key: String) {
        let had = lock.withLock { () -> Bool in
            let exists = cache[key] != nil
            cache[key] = nil
            return exists
        }
        if had { Keychain.delete(key: key) }
    }
}

// MARK: - Chat provider selection

enum ChatProvider: String, CaseIterable, Identifiable {
    case anthropic, openai, claudeCLI
    var id: String { rawValue }

    var label: String {
        switch self {
        case .anthropic: return "Anthropic API"
        case .openai:    return "OpenAI-compatible (DeepSeek, Ollama, OpenRouter…)"
        case .claudeCLI: return "Claude subscription (local claude CLI)"
        }
    }

    static var current: ChatProvider {
        get { ChatProvider(rawValue: UserDefaults.standard.string(forKey: "chatProvider") ?? "") ?? .anthropic }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: "chatProvider") }
    }

    static var openAIBaseURL: String {
        UserDefaults.standard.string(forKey: "openaiBaseURL") ?? "https://api.deepseek.com"
    }
    static var openAIModel: String {
        UserDefaults.standard.string(forKey: "openaiModel") ?? "deepseek-chat"
    }
}

// MARK: - Claude API

@MainActor
final class ClaudeService {
    static let shared = ClaudeService()

    private let endpoint = URL(string: "https://api.anthropic.com/v1/messages")!
    private let anthropicVersion = "2023-06-01"
    private let model = "claude-sonnet-4-6"

    var apiKey: String? { KeychainStore.shared.get("anthropic-api-key") }

    // Multi-turn conversation messages (for API)
    private var conversationMessages: [[String: Any]] = []

    // Provider-neutral history for OpenAI-compatible providers
    private var plainMessages: [[String: String]] = []
    // Session id of the local `claude` CLI conversation
    private var cliSessionId: String?

    func clearConversation() {
        conversationMessages = []
        plainMessages = []
        cliSessionId = nil
    }

    /// True when the selected chat provider can be used right now.
    var isConfigured: Bool {
        switch ChatProvider.current {
        case .anthropic: return !(apiKey ?? "").isEmpty
        case .openai:    return !ChatProvider.openAIBaseURL.isEmpty
        case .claudeCLI: return true
        }
    }

    private let systemPrompt = """
    You are Mochi, Louis's personal AI assistant embedded in the notch of his Mac. \
    You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
    Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
    No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.
    """

    private let webSearchTools: [[String: Any]] = [
        ["type": "web_search_20250305", "name": "web_search", "max_uses": 5]
    ]

    // MARK: - Chat (multi-turn, natural text + web search)

    func chat(query: String, context: PromptContext?, state: AppState) async {
        switch ChatProvider.current {
        case .anthropic: break
        case .openai:
            await chatOpenAI(query: query, context: context, state: state)
            return
        case .claudeCLI:
            await chatCLI(query: query, context: context, state: state)
            return
        }
        guard let key = apiKey, !key.isEmpty else {
            await showError("API key missing. Open settings.", state: state)
            return
        }

        // Build user content for this turn
        var userContent: [[String: Any]] = []

        // Add file/window context on first message only
        if conversationMessages.isEmpty, let context = context {
            switch context {
            case .window(let app, let title, let url):
                var text = "Context — App: \(app), Window: \(title)"
                if let url = url { text += ", URL: \(url)" }
                userContent.append(["type": "text", "text": text])
            case .file(let name, let fileURL):
                if let fileURL = fileURL, let block = readFileAsBlock(url: fileURL) {
                    userContent.append(block)
                }
                userContent.append(["type": "text", "text": "File: \(name)"])
            }
        }
        userContent.append(["type": "text", "text": query])

        conversationMessages.append(["role": "user", "content": userContent])

        let body: [String: Any] = [
            "model": model,
            "max_tokens": 4096,
            "tools": webSearchTools,
            "system": systemPrompt,
            "messages": conversationMessages,
        ]

        do {
            let data = try await callAPI(body: body, key: key, beta: "web-search-2025-03-05")
            await handleChatResult(data, state: state)
        } catch {
            conversationMessages.removeLast()
            await showError("Network error: \(error.localizedDescription)", state: state)
        }
    }

    // MARK: - Structured search (M8 — window attach + web search)

    func search(query: String, context: PromptContext?, state: AppState) async {
        guard let key = apiKey, !key.isEmpty else {
            await showError("Anthropic API key missing. Open settings to configure it.", state: state)
            return
        }

        var userContent: [[String: Any]] = []
        switch context {
        case .window(let appName, let title, let url):
            var text = "App: \(appName)\nWindow title: \(title)"
            if let url = url { text += "\nURL: \(url)" }
            text += "\n\nRequest: \(query)"
            userContent.append(["type": "text", "text": text])
        case .file(let name, let fileURL):
            if let fileURL = fileURL, let fileBlock = readFileAsBlock(url: fileURL) {
                userContent.append(fileBlock)
            }
            userContent.append(["type": "text", "text": "File: \(name)\n\nRequest: \(query)"])
        case nil:
            userContent.append(["type": "text", "text": query])
        }

        let system = """
        You are an assistant built into the notch of a Mac. Reply in English, short and precise.
        Reply ONLY with valid JSON in this exact format:
        {"title":"...","items":[{"label":"...","detail":"...","url":"..."}],"note":"..."}
        Maximum 3 items. "url" is optional. "note" is optional.
        """

        let tools: [[String: Any]] = [
            ["type": "web_search_20250305", "name": "web_search", "max_uses": 3]
        ]

        let body: [String: Any] = [
            "model": model,
            "max_tokens": 1024,
            "tools": tools,
            "system": system,
            "messages": [["role": "user", "content": userContent]],
        ]

        do {
            let result = try await callAPI(body: body, key: key, beta: "web-search-2025-03-05")
            await handleResult(result, state: state)
        } catch {
            await showError("Network error: \(error.localizedDescription)", state: state)
        }
    }

    // MARK: - API call

    private func callAPI(body: [String: Any], key: String, beta: String? = nil) async throws -> Data {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue(key, forHTTPHeaderField: "x-api-key")
        request.setValue(anthropicVersion, forHTTPHeaderField: "anthropic-version")
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        if let beta { request.setValue(beta, forHTTPHeaderField: "anthropic-beta") }
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        request.timeoutInterval = 45

        let (data, response) = try await URLSession.shared.data(for: request)

        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let msg = String(data: data, encoding: .utf8) ?? "unknown error"
            throw NSError(domain: "Claude", code: 0, userInfo: [NSLocalizedDescriptionKey: msg])
        }
        return data
    }

    // MARK: - Chat result handler

    private func handleChatResult(_ data: Data, state: AppState) async {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]] else {
            await showError("Unexpected API response.", state: state)
            return
        }

        // Store full content (includes tool_use/tool_result blocks) for correct multi-turn context
        conversationMessages.append(["role": "assistant", "content": content])

        guard let textBlock = content.first(where: { $0["type"] as? String == "text" }),
              let text = textBlock["text"] as? String, !text.isEmpty else {
            await showError("No response text.", state: state)
            return
        }

        deliver(text: text, state: state)
    }

    private func deliver(text: String, state: AppState) {
        state.chatHistory.append(ChatMessage(role: .assistant, content: text.trimmingCharacters(in: .whitespacesAndNewlines)))
        state.stateOverride = nil
        state.view = .prompt
        NotificationCenter.default.post(name: .triggerEmote, object: BotEmote.happy)
    }

    // MARK: - Plain-text context (providers without Anthropic content blocks)

    private func plainContext(_ context: PromptContext?, attachFilePath: Bool) -> String? {
        guard let context else { return nil }
        switch context {
        case .window(let app, let title, let url):
            var text = "Context — App: \(app), Window: \(title)"
            if let url { text += ", URL: \(url)" }
            return text
        case .file(let name, let fileURL):
            if let fileURL, let block = readFileAsBlock(url: fileURL),
               block["type"] as? String == "text", let t = block["text"] as? String {
                return "File: \(name)\n\(t)"
            }
            if attachFilePath, let fileURL { return "File: \(fileURL.path)" }
            return "File: \(name) (content not available with this provider)"
        }
    }

    // MARK: - OpenAI-compatible chat (DeepSeek, OpenRouter, Groq, Ollama…)

    private func chatOpenAI(query: String, context: PromptContext?, state: AppState) async {
        var userText = query
        if plainMessages.isEmpty, let ctx = plainContext(context, attachFilePath: false) {
            userText = ctx + "\n\n" + query
        }
        plainMessages.append(["role": "user", "content": userText])

        let base = ChatProvider.openAIBaseURL.trimmingCharacters(in: CharacterSet(charactersIn: "/ "))
        guard let url = URL(string: base + "/chat/completions") else {
            plainMessages.removeLast()
            await showError("Invalid base URL in settings.", state: state)
            return
        }
        let system = systemPrompt.replacingOccurrences(of: "You have web search access and ", with: "You ")
            + " You have no web access."
        let body: [String: Any] = [
            "model": ChatProvider.openAIModel,
            "max_tokens": 4096,
            "messages": [["role": "system", "content": system]] + plainMessages,
        ]

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        if let key = KeychainStore.shared.get("openai-compat-key"), !key.isEmpty {
            request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        }
        request.timeoutInterval = 90

        do {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
                plainMessages.removeLast()
                await showError(String(data: data, encoding: .utf8) ?? "Request failed.", state: state)
                return
            }
            guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let choices = json["choices"] as? [[String: Any]],
                  let message = choices.first?["message"] as? [String: Any],
                  let text = message["content"] as? String, !text.isEmpty else {
                plainMessages.removeLast()
                await showError("Unexpected API response.", state: state)
                return
            }
            plainMessages.append(["role": "assistant", "content": text])
            deliver(text: text, state: state)
        } catch {
            plainMessages.removeLast()
            await showError("Network error: \(error.localizedDescription)", state: state)
        }
    }

    // MARK: - Claude subscription via the local `claude` CLI

    private func chatCLI(query: String, context: PromptContext?, state: AppState) async {
        #if APPSTORE
        await showError("Claude CLI is not available in the sandboxed App Store build.", state: state)
        #else
        var prompt = query
        if cliSessionId == nil, let ctx = plainContext(context, attachFilePath: true) {
            prompt = ctx + "\n\n" + query
        }
        let session = cliSessionId
        let system = systemPrompt
        let result = await Task.detached {
            Self.runClaudeCLI(prompt: prompt, system: system, session: session)
        }.value
        switch result {
        case .success(let reply):
            if let newSession = reply.session { cliSessionId = newSession }
            deliver(text: reply.text, state: state)
        case .failure(let error):
            await showError(error.localizedDescription, state: state)
        }
        #endif
    }

    #if !APPSTORE
    nonisolated private static func runClaudeCLI(prompt: String, system: String, session: String?) -> Result<(text: String, session: String?), NSError> {
        func fail(_ m: String) -> Result<(text: String, session: String?), NSError> {
            .failure(NSError(domain: "ClaudeCLI", code: 1, userInfo: [NSLocalizedDescriptionKey: m]))
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/zsh")
        // Login shell so `claude` is found on the user's PATH; values travel via env, never interpolated into the script.
        process.arguments = ["-lc", #"claude -p "$COUCOU_PROMPT" --output-format json --append-system-prompt "$COUCOU_SYSTEM" ${COUCOU_SESSION:+--resume "$COUCOU_SESSION"}"#]
        var env = ProcessInfo.processInfo.environment
        env["COUCOU_PROMPT"] = prompt
        env["COUCOU_SYSTEM"] = system
        env["COUCOU_SESSION"] = session ?? ""
        process.environment = env
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return fail("Cannot launch claude: \(error.localizedDescription)") }
        DispatchQueue.global().asyncAfter(deadline: .now() + 180) { if process.isRunning { process.terminate() } }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        let errData = err.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let text = json["result"] as? String else {
            let msg = String(data: errData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return fail(msg.isEmpty ? "claude CLI not found or not logged in (run `claude` once in Terminal)." : msg)
        }
        if json["is_error"] as? Bool == true { return fail(text) }
        return .success((text, json["session_id"] as? String))
    }
    #endif

    // MARK: - Structured result handler

    private func handleResult(_ data: Data, state: AppState) async {
        // Extract text from Anthropic response (may contain tool_use / web_search_tool_result blocks)
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]],
              let textBlock = content.first(where: { $0["type"] as? String == "text" }),
              let text = textBlock["text"] as? String else {
            await showError("Unexpected API response.", state: state)
            return
        }

        // Strip markdown code fences if present, then extract JSON object
        let cleanText: String
        if let start = text.firstIndex(of: "{"), let end = text.lastIndex(of: "}") {
            cleanText = String(text[start...end])
        } else {
            cleanText = text
        }

        // Try to parse as our JSON format
        if let resultData = cleanText.data(using: .utf8),
           let parsed = try? JSONSerialization.jsonObject(with: resultData) as? [String: Any] {
            let title  = parsed["title"] as? String ?? "Result"
            let note   = parsed["note"] as? String
            var items: [ResultItem] = []
            if let rawItems = parsed["items"] as? [[String: Any]] {
                for item in rawItems.prefix(3) {
                    items.append(ResultItem(
                        label:  item["label"]  as? String ?? "",
                        detail: item["detail"] as? String ?? "",
                        url:    item["url"]    as? String
                    ))
                }
            }
            state.searchResult = SearchResult(title: title, items: items, note: note)
        } else {
            // Fallback: show raw text in 3-line chunks
            let lines = cleanText.components(separatedBy: "\n").filter { !$0.isEmpty }.prefix(3)
            state.searchResult = SearchResult(
                title: "Claude's response",
                items: lines.map { ResultItem(label: $0, detail: "", url: nil) },
                note: nil
            )
        }

        state.stateOverride = nil
        state.view = .result
        NotificationCenter.default.post(name: .triggerEmote, object: BotEmote.proud)
    }

    private func showError(_ message: String, state: AppState) async {
        state.stateOverride = .error
        state.noteMessage = message
        state.view = .note
    }

    // MARK: - File content block builder

    private func readFileAsBlock(url: URL) -> [String: Any]? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        let ext = url.pathExtension.lowercased()
        let base64 = data.base64EncodedString()

        if ext == "pdf" {
            return ["type": "document", "source": ["type": "base64", "media_type": "application/pdf", "data": base64]]
        } else if ["jpg", "jpeg"].contains(ext) {
            return ["type": "image", "source": ["type": "base64", "media_type": "image/jpeg", "data": base64]]
        } else if ext == "png" {
            return ["type": "image", "source": ["type": "base64", "media_type": "image/png", "data": base64]]
        } else if ext == "gif" {
            return ["type": "image", "source": ["type": "base64", "media_type": "image/gif", "data": base64]]
        } else if ext == "webp" {
            return ["type": "image", "source": ["type": "base64", "media_type": "image/webp", "data": base64]]
        } else {
            // Text/code — inline as text if <= 200 KB
            guard data.count <= 200_000,
                  let text = String(data: data, encoding: .utf8) else { return nil }
            return ["type": "text", "text": "File contents:\n\(text)"]
        }
    }
}
