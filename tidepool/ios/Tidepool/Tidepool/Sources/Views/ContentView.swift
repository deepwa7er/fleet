import SwiftUI
import UIKit

struct ContentView: View {
    @State private var store = ClipStore()
    @State private var showingSettings = false

    var body: some View {
        NavigationStack {
            content
                .navigationTitle("Tidepool")
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button {
                            showingSettings = true
                        } label: {
                            Image(systemName: "gearshape")
                        }
                        .accessibilityLabel("Settings")
                    }
                }
                .refreshable {
                    await store.refresh()
                }
                .task {
                    await store.refresh()
                }
                .sheet(isPresented: $showingSettings) {
                    SettingsView(store: store)
                }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let clip = store.clip {
            clipView(clip)
        } else if let error = store.error {
            errorView(error)
        } else if store.isLoading {
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            emptyView
        }
    }

    private func clipView(_ clip: Clip) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                metaRow(clip)
                Divider()
                Text(clip.text.isEmpty ? "(empty clip)" : clip.text)
                    .font(.body.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button {
                    UIPasteboard.general.string = clip.text
                } label: {
                    Label("Copy to iOS clipboard", systemImage: "doc.on.doc")
                }
                .buttonStyle(.borderedProminent)
                .disabled(clip.text.isEmpty)
                if let error = store.error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
            .padding()
        }
    }

    private func metaRow(_ clip: Clip) -> some View {
        HStack {
            Text(clip.updatedBy)
                .font(.footnote.weight(.medium))
            Spacer()
            Text(clip.updatedAt, format: .relative(presentation: .named))
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }

    private func errorView(_ message: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.largeTitle)
                .foregroundStyle(.orange)
            Text(message)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            Button("Retry") {
                Task { await store.refresh() }
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyView: some View {
        VStack(spacing: 8) {
            Text("No clip yet")
                .font(.headline)
            Text("Push something from another device, or pull to refresh.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    ContentView()
}
