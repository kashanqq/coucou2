import AppKit
import SwiftUI

// MARK: - Calendar view
//
// The week ahead from Google Calendar, grouped by day. Mochi sits on the left
// like in the chat; the list scrolls on the right and opens on what is
// happening now rather than on this morning's finished meetings.

struct CalendarIslandView: View {
    @ObservedObject var state: AppState
    @State private var syncing = false

    var body: some View {
        ZStack(alignment: .topLeading) {
            CardBackground(wash: nil)
            // Re-renders once a minute for "now" / "in 5 min" — only while the
            // island is open, since this view only exists then.
            TimelineView(.periodic(from: .now, by: 60)) { context in
                VStack(alignment: .leading, spacing: 4) {
                    header(now: context.date)
                    content(now: context.date)
                }
                .padding(.leading, 84)
                .padding(.trailing, 12)
                .padding(.vertical, 10)
            }
        }
    }

    // MARK: Header

    private func header(now: Date) -> some View {
        HStack(spacing: 7) {
            Circle().fill(Color(hex: "#4285F4")).frame(width: 7, height: 7)
            Text("Google Calendar")
                .font(.system(size: 12, weight: .semibold))
                .foregroundColor(Color(hex: "#8E939C"))
            if state.calendarConnected, let synced = state.calendarSyncedAt {
                Text("· \(syncedLabel(synced, now: now))")
                    .font(.system(size: 11.5))
                    .foregroundColor(Color(hex: "#6B7079"))
            }
            Spacer()
            if state.calendarConnected {
                iconButton("arrow.clockwise", help: "Sync now") {
                    SoundEngine.shared.play("blip")
                    syncing = true
                    Task { @MainActor in
                        await GoogleCalendarService.shared.sync()
                        syncing = false
                    }
                }
                .rotationEffect(.degrees(syncing ? 360 : 0))
                .animation(syncing ? .linear(duration: 0.8).repeatForever(autoreverses: false) : .default,
                           value: syncing)
            }
            iconButton("arrow.up.right", help: "Open Google Calendar") {
                if let url = URL(string: "https://calendar.google.com") { NSWorkspace.shared.open(url) }
            }
        }
    }

    private func iconButton(_ symbol: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 9, weight: .semibold))
                .foregroundColor(Color(hex: "#8E939C"))
                .frame(width: 20, height: 20)
                .background(Color.white.opacity(0.07))
                .clipShape(Circle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    // MARK: Content

    @ViewBuilder
    private func content(now: Date) -> some View {
        if !state.calendarConnected {
            VStack(alignment: .leading, spacing: 6) {
                Text("Google Calendar isn't connected.")
                    .font(.system(size: 15, weight: .semibold))
                Text("Add your OAuth client in Settings, then click Connect.")
                    .font(.system(size: 13))
                    .foregroundColor(Color(hex: "#9398A1"))
                Button("Open Settings") {
                    NotificationCenter.default.post(name: .openFullSettings, object: nil)
                }
                .buttonStyle(PrimaryCalendarButton())
                .padding(.top, 4)
            }
            .padding(.top, 14)
        } else {
            let days = groupByDay(state.calendarEvents, now: now)
            if let error = state.calendarError {
                Text(error)
                    .font(.system(size: 12))
                    .foregroundColor(Color(hex: "#FF8D97"))
                    .lineLimit(2)
            }
            if days.isEmpty {
                Text(state.calendarSyncedAt == nil ? "Syncing…" : "Nothing on your calendar this week.")
                    .font(.system(size: 15, weight: .semibold))
                    .padding(.top, 14)
            } else {
                eventList(days: days, now: now)
            }
        }
    }

    private func eventList(days: [CalendarDayGroup], now: Date) -> some View {
        let firstLive = days.lazy.flatMap { $0.events }.first { $0.end > now }?.id
        return ScrollViewReader { proxy in
            ScrollView(.vertical, showsIndicators: true) {
                LazyVStack(alignment: .leading, spacing: 2, pinnedViews: [.sectionHeaders]) {
                    ForEach(days, id: \.day) { group in
                        Section(header: dayHeader(group.day, now: now)) {
                            ForEach(group.events) { event in
                                CalendarEventRow(event: event, now: now)
                                    .id("\(group.day.timeIntervalSince1970)-\(event.id)")
                            }
                        }
                    }
                }
            }
            .onAppear { scroll(proxy, to: firstLive, in: days) }
            .onChange(of: state.calendarSyncedAt) { _, _ in scroll(proxy, to: firstLive, in: days) }
        }
    }

    private func scroll(_ proxy: ScrollViewProxy, to id: String?, in days: [CalendarDayGroup]) {
        guard let id, let day = days.first(where: { $0.events.contains { $0.id == id } })?.day else { return }
        DispatchQueue.main.async {
            proxy.scrollTo("\(day.timeIntervalSince1970)-\(id)", anchor: .top)
        }
    }

    private func dayHeader(_ day: Date, now: Date) -> some View {
        let cal = Calendar.current
        let today = cal.startOfDay(for: now)
        let formatted = Self.dayFormatter.string(from: day)
        let label: String
        if day == today { label = "Today · \(formatted)" }
        else if day == cal.date(byAdding: .day, value: 1, to: today) { label = "Tomorrow · \(formatted)" }
        else { label = formatted }
        return Text(label.uppercased())
            .font(.system(size: 10.5, weight: .semibold))
            .kerning(0.4)
            .foregroundColor(Color(hex: "#6B7079"))
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 6)
            .padding(.bottom, 3)
            .background(Color(hex: "#141518"))
    }

    // MARK: Helpers

    private static let dayFormatter: DateFormatter = {
        let f = DateFormatter()
        f.setLocalizedDateFormatFromTemplate("EEEEdMMM")
        return f
    }()

    private func syncedLabel(_ date: Date, now: Date) -> String {
        let s = max(0, now.timeIntervalSince(date))
        if s < 60 { return "synced just now" }
        let m = Int((s / 60).rounded())
        return m < 60 ? "synced \(m) min ago" : "synced \(m / 60) h ago"
    }

    /// Events from the start of today on, each placed on every day it covers.
    private func groupByDay(_ events: [CalendarEvent], now: Date) -> [CalendarDayGroup] {
        let cal = Calendar.current
        let today = cal.startOfDay(for: now)
        var byDay: [Date: [CalendarEvent]] = [:]
        for e in events where e.end > today {
            var day = max(cal.startOfDay(for: e.start), today)
            // All-day end dates are exclusive; a multi-day event shows on each day.
            let last = e.end.addingTimeInterval(-1)
            var guardCount = 0
            while day <= last && guardCount < 9 {
                byDay[day, default: []].append(e)
                guard let next = cal.date(byAdding: .day, value: 1, to: day) else { break }
                day = next
                guardCount += 1
            }
        }
        return byDay.keys.sorted().map { day in
            CalendarDayGroup(day: day, events: byDay[day]!.sorted {
                $0.start == $1.start ? ($0.allDay && !$1.allDay) : $0.start < $1.start
            })
        }
    }
}

// MARK: - Event row

struct CalendarEventRow: View {
    let event: CalendarEvent
    let now: Date
    @State private var hovered = false

    private static let interval: DateIntervalFormatter = {
        let f = DateIntervalFormatter()
        f.dateStyle = .none
        f.timeStyle = .short
        return f
    }()

    private var live: Bool { !event.allDay && event.start <= now && now < event.end }
    private var past: Bool { event.end <= now }
    private var minutesUntil: Int? {
        guard !event.allDay, event.start > now else { return nil }
        let m = event.start.timeIntervalSince(now) / 60
        return m <= 15 ? max(1, Int(m.rounded())) : nil
    }

    var body: some View {
        HStack(spacing: 9) {
            RoundedRectangle(cornerRadius: 2)
                .fill(Color(hex: event.color))
                .frame(width: 3, height: 26)
            Text(event.allDay ? "All day" : Self.interval.string(from: event.start, to: event.end))
                .font(.system(size: 11.5, weight: .medium))
                .monospacedDigit()
                .foregroundColor(Color(hex: "#9398A1"))
                .frame(minWidth: 92, alignment: .leading)
            VStack(alignment: .leading, spacing: 1) {
                Text(event.title)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(Color(hex: "#F5F6F8"))
                    .lineLimit(1)
                if let location = event.location, !location.isEmpty {
                    Text(location)
                        .font(.system(size: 11))
                        .foregroundColor(Color(hex: "#6B7079"))
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 4)
            if live {
                badge("now", fg: .white, bg: Color(hex: "#4285F4"))
            } else if let m = minutesUntil {
                badge("in \(m) min", fg: Color(hex: "#F5A524"), bg: Color(hex: "#F5A524").opacity(0.2))
            }
            if let meet = event.meetURL, !past, let url = URL(string: meet) {
                Button("Join") { NSWorkspace.shared.open(url) }
                    .buttonStyle(PrimaryCalendarButton())
            }
        }
        .padding(.vertical, 4)
        .padding(.leading, 6)
        .padding(.trailing, 8)
        .frame(minHeight: 34)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(live ? Color(hex: "#4285F4").opacity(0.14)
                      : hovered ? Color.white.opacity(0.06) : Color.clear)
        )
        .opacity(past ? 0.42 : 1)
        .contentShape(Rectangle())
        .onHover { hovered = $0 }
        .onTapGesture {
            if let link = event.htmlLink, let url = URL(string: link) { NSWorkspace.shared.open(url) }
        }
        .help(event.calendar)
    }

    private func badge(_ text: String, fg: Color, bg: Color) -> some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .foregroundColor(fg)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(bg)
            .clipShape(Capsule())
    }
}

struct PrimaryCalendarButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11.5, weight: .medium))
            .foregroundColor(Color(hex: "#0B0C0E"))
            .padding(.horizontal, 11)
            .padding(.vertical, 4)
            .background(Color(hex: "#F5F6F8"))
            .clipShape(Capsule())
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
    }
}

/// One day of the list: its local midnight and the events that touch it.
struct CalendarDayGroup: Identifiable {
    let day: Date
    let events: [CalendarEvent]
    var id: Date { day }
}
