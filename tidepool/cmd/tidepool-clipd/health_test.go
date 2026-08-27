package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func testHealthStatus() healthStatus {
	return healthStatus{
		Protocol:     healthProtocol,
		PID:          42,
		Instance:     strings.Repeat("1a", 16),
		BinarySHA256: strings.Repeat("2b", 32),
		Ready:        true,
	}
}

func shortTempDir(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "clipd-health-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

func TestHealthServerRoundTrip(t *testing.T) {
	path := filepath.Join(shortTempDir(t), "health", "clipd.sock")
	server, err := startHealthServer(path, testHealthStatus())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- server.serve(ctx) }()

	queryCtx, queryCancel := context.WithTimeout(context.Background(), time.Second)
	got, err := queryHealth(queryCtx, path)
	queryCancel()
	if err != nil {
		t.Fatal(err)
	}
	if got != testHealthStatus() {
		t.Fatalf("health status = %#v, want %#v", got, testHealthStatus())
	}

	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if _, err := os.Lstat(path); !os.IsNotExist(err) {
		t.Fatalf("health socket still exists after shutdown: %v", err)
	}
}

func TestHealthServerReplacesStaleSocket(t *testing.T) {
	path := filepath.Join(shortTempDir(t), "clipd.sock")
	stale, err := startHealthServer(path, testHealthStatus())
	if err != nil {
		t.Fatal(err)
	}
	if err := stale.listener.Close(); err != nil {
		t.Fatal(err)
	}

	replacement, err := startHealthServer(path, testHealthStatus())
	if err != nil {
		t.Fatal(err)
	}
	stale.removeOwnedSocket()
	if _, err := os.Lstat(path); err != nil {
		t.Fatalf("stale server removed replacement socket: %v", err)
	}
	replacement.listener.Close()
	os.Remove(path)
}

func TestHealthServerRefusesAnActiveDaemon(t *testing.T) {
	path := filepath.Join(shortTempDir(t), "clipd.sock")
	server, err := startHealthServer(path, testHealthStatus())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- server.serve(ctx) }()

	_, err = startHealthServer(path, testHealthStatus())
	if err == nil || !strings.Contains(err.Error(), "another tidepool-clipd") {
		t.Fatalf("startHealthServer error = %v", err)
	}
	queryCtx, queryCancel := context.WithTimeout(context.Background(), time.Second)
	if _, err := queryHealth(queryCtx, path); err != nil {
		t.Fatalf("original health server was disrupted: %v", err)
	}
	queryCancel()

	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func TestHealthServerRefusesNonSocketPath(t *testing.T) {
	path := filepath.Join(shortTempDir(t), "clipd.sock")
	if err := os.WriteFile(path, []byte("do not delete"), 0o600); err != nil {
		t.Fatal(err)
	}

	_, err := startHealthServer(path, testHealthStatus())
	if err == nil || !strings.Contains(err.Error(), "is not a socket") {
		t.Fatalf("startHealthServer error = %v", err)
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(contents) != "do not delete" {
		t.Fatalf("non-socket path was changed: %q", contents)
	}
}

func TestQueryHealthRejectsUnreadyDaemon(t *testing.T) {
	path := filepath.Join(shortTempDir(t), "clipd.sock")
	status := testHealthStatus()
	status.Ready = false
	server, err := startHealthServer(path, status)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- server.serve(ctx) }()

	queryCtx, queryCancel := context.WithTimeout(context.Background(), time.Second)
	_, err = queryHealth(queryCtx, path)
	queryCancel()
	if err == nil || !strings.Contains(err.Error(), "not ready") {
		t.Fatalf("queryHealth error = %v", err)
	}

	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}
